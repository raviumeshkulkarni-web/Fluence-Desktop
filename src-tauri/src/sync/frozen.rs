// Fluence sync — frozen v1.2 domain engine (dictionary, snippets, stats, settings)
//
// Drive layout: appDataFolder/fluence/v1/{dictionary.json,snippets.json,stats.json,settings.json}
// Clock: wall UTC ms + persisted maxSeen floor; winner = max(updatedAt, deviceId).
// Tombstones are ordinary records: they win exactly when they are newest, so a
// later re-creation legitimately beats an older deletion.
//
// Per-domain pass (identical shape for all four domains):
//   stamp unstamped local rows -> LIST(id,version) -> GET content -> MERGE
//   -> PUT(expected_version) with staleness re-check loop
//
// Concurrency: Drive v3 does not honor If-Match, so freshness is verified via
// the file `version` revision immediately before write. On `StaleVersion` the
// full GET->MERGE cycle reruns against the fresh remote (bounded retries).
// Check-then-write is not atomic; a race inside that window heals on the next
// pass because every device persists its merged state locally. No silent loss:
// local data is never discarded unless a strictly newer remote record wins.
//
// Corruption isolation: an unparseable or oversized remote envelope is skipped
// (treated as absent) — one bad domain never blocks the others.

use crate::sync::domain::*;
use crate::sync::drive::{
    DomainDriveStore, DomainFileMeta, DICT_FILE, SETTINGS_FILE, SNIPPETS_FILE, STATS_FILE,
};
use crate::sync::error::SyncError;
use crate::sync::merge::{self, MergeOutcome};
use crate::sync::metadata::SyncMetadata;

/// Result of a domain sync pass.
#[derive(Debug, Default, Clone)]
pub struct DomainSyncOutcome {
    pub pushed: bool,
    pub merged: bool,
    pub items_merged: usize,
}

/// Local store seam for one domain. After a successful PUT the engine replaces
/// the account's local set with the merged winners (`save_merged`) and marks
/// them clean (`mark_all_pushed`) — one transactional write per domain.
pub trait DirtyStore {
    type Item: Clone + PartialEq;

    /// Rows owned by this account (live + tombstones).
    fn load(&self, account_hash: &str) -> Vec<Self::Item>;
    /// Stamp rows with no account ownership into this account (first login /
    /// enrollment). Returns how many rows were stamped.
    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError>;
    fn has_dirty(&self, account_hash: &str) -> bool;
    /// Replace this account's rows with the merged winners.
    fn save_merged(&mut self, account_hash: &str, merged: Vec<Self::Item>)
        -> Result<(), SyncError>;
    /// After a confirmed push every merged row is clean and pushed.
    fn mark_all_pushed(&mut self, account_hash: &str) -> Result<(), SyncError>;
    /// Drop locally-created tombstones that were never uploaded (nothing to
    /// propagate). Default: no-op (settings/stats have no tombstones).
    fn hard_delete_never_pushed_tombstones(
        &mut self,
        account_hash: &str,
    ) -> Result<usize, SyncError> {
        let _ = account_hash;
        Ok(0)
    }
}

/// Maximum GET->MERGE->PUT cycles per domain per pass. Attempt 1 plus three
/// staleness retries; exhaustion surfaces as retryable so the scheduler
/// backs off and the next pass resumes from fresh state.
const MAX_ATTEMPTS: usize = 4;

/// One generic domain sync pass. All four domains differ only in their item
/// type, merge law, codec and ordering key — captured here as parameters.
fn sync_domain<T>(
    drive: &mut dyn DomainDriveStore,
    name: &str,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = T>,
    merge_fn: fn(&[T], &[T]) -> MergeOutcome<T>,
    encode: fn(&[T]) -> Vec<u8>,
    decode: fn(&[u8]) -> Option<Vec<T>>,
    sort_items: fn(&mut [T]),
    ts_of: fn(&T) -> i64,
) -> Result<DomainSyncOutcome, SyncError>
where
    T: Clone + PartialEq,
{
    // 1. Enrollment: claim unstamped local rows into this account first so
    // pre-existing local content flows UP to the account on first sign-in.
    store.stamp_account(account_hash)?;
    let local_items = store.load(account_hash);

    let mut attempts = 0usize;
    while attempts < MAX_ATTEMPTS {
        attempts += 1;

        // 2. Read remote state (all duplicate files merged; corrupt skipped).
        let files = drive.list_v1_files()?;
        let mut domain_files: Vec<&DomainFileMeta> =
            files.iter().filter(|f| f.name == name).collect();
        domain_files.sort_by(|a, b| a.file_id.cmp(&b.file_id));
        let preferred_file_id = domain_files.first().map(|m| m.file_id.as_str());
        let mut remote_items: Vec<T> = Vec::new();
        let mut remote_version: Option<String> = None;
        let mut valid_found = false;
        for meta in &domain_files {
            let bytes = match drive.get_domain_content(&meta.file_id)? {
                Some(b) => b,
                None => continue,
            };
            if let Some(items) = decode(&bytes) {
                remote_items.extend(items);
                if remote_version.is_none() {
                    remote_version = meta.version.clone();
                }
                valid_found = true;
            }
            // Corrupt envelope: skip this file, keep its siblings.
        }

        // 3. Deterministic merge (pure LWW).
        let outcome = merge_fn(&local_items, &remote_items);
        let mut merged = outcome.merged;
        sort_items(&mut merged);

        // 4. Push decision: push when merged state differs from what we read,
        // or when local rows are dirty. The state-difference term is what
        // heals a lost concurrent race on the NEXT pass (our items may be
        // locally CLEAN yet absent from the remote file another device
        // overwrote us with).
        let mut remote_sorted = remote_items.clone();
        sort_items(&mut remote_sorted);
        let state_differs = merged != remote_sorted;
        let needs_push =
            state_differs || store.has_dirty(account_hash) || (!valid_found && !merged.is_empty());

        if !needs_push {
            if outcome.changed {
                let max_ts = merged.iter().map(ts_of).max().unwrap_or(0);
                // Same lock discipline as the stamped store paths: the
                // persisted max_seen read-modify-write must not race them.
                let _io = crate::sync::io_lock::io_lock_guard();
                metadata.update_max_seen(account_hash, max_ts);
                let count = merged.len();
                store.save_merged(account_hash, merged)?;
                return Ok(DomainSyncOutcome {
                    pushed: false,
                    merged: true,
                    items_merged: count,
                });
            }
            return Ok(DomainSyncOutcome {
                pushed: false,
                merged: false,
                items_merged: merged.len(),
            });
        }

        // 5. Never upload state we cannot re-parse (roundtrip validation).
        let bytes = encode(&merged);
        if decode(&bytes) != Some(merged.clone()) {
            return Err(SyncError::Fatal(format!(
                "{name}: serialized envelope failed roundtrip validation; upload refused"
            )));
        }

        // 6. Advance the monotonic clock floor before writing (under io_lock,
        // like every other max_seen writer; the guard is reentrant).
        let _io = crate::sync::io_lock::io_lock_guard();
        let max_ts = merged.iter().map(ts_of).max().unwrap_or(0);
        metadata.update_max_seen(account_hash, max_ts);
        drop(_io);

        // 7. Version-checked upload. StaleVersion loops back to step 2.
        match drive.put_domain(name, &bytes, remote_version.as_deref(), preferred_file_id) {
            Ok(new_version) => {
                metadata.set_last_rev(account_hash, name, new_version);
                let count = merged.len();
                store.save_merged(account_hash, merged)?;
                store.mark_all_pushed(account_hash)?;
                store.hard_delete_never_pushed_tombstones(account_hash)?;
                // Consolidate duplicate domain files: keep the first valid
                // one (the file we just updated), drop extras.
                if domain_files.len() > 1 {
                    if let Some(target) = preferred_file_id {
                        for dup in domain_files.iter().filter(|d| d.file_id != target) {
                            let _ = drive.delete_domain_file(&dup.file_id);
                        }
                    } else {
                        for dup in domain_files.iter().skip(1) {
                            let _ = drive.delete_domain_file(&dup.file_id);
                        }
                    }
                }
                return Ok(DomainSyncOutcome {
                    pushed: true,
                    merged: outcome.changed,
                    items_merged: count,
                });
            }
            Err(SyncError::StaleVersion(live)) => {
                log::debug!("sync: {name} changed under us (live={live}); re-fetching");
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(SyncError::Retryable(format!(
        "{name}: state kept changing during sync ({MAX_ATTEMPTS} attempts)"
    )))
}

// ── Thin per-domain wrappers ────────────────────────────────────────────────

pub fn sync_dictionary_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = DictionaryItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    sync_domain(
        drive,
        DICT_FILE,
        account_hash,
        metadata,
        store,
        merge::merge_dictionary,
        |items| {
            DictionaryEnvelope {
                v: ENVELOPE_V1,
                entries: items.to_vec(),
            }
            .to_bytes()
        },
        |bytes| DictionaryEnvelope::from_bytes(bytes).map(|e| e.entries),
        |items| {
            items.sort_by(|a, b| {
                a.business_key()
                    .cmp(&b.business_key())
                    .then_with(|| a.sync_id.cmp(&b.sync_id))
            })
        },
        |i| i.updated_at,
    )
}

pub fn sync_snippet_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = SnippetItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    sync_domain(
        drive,
        SNIPPETS_FILE,
        account_hash,
        metadata,
        store,
        merge::merge_snippets,
        |items| {
            SnippetEnvelope {
                v: ENVELOPE_V1,
                entries: items.to_vec(),
            }
            .to_bytes()
        },
        |bytes| SnippetEnvelope::from_bytes(bytes).map(|e| e.entries),
        |items| {
            items.sort_by(|a, b| {
                a.business_key()
                    .cmp(&b.business_key())
                    .then_with(|| a.sync_id.cmp(&b.sync_id))
            })
        },
        |i| i.updated_at,
    )
}

pub fn sync_settings_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = SettingsItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    sync_domain(
        drive,
        SETTINGS_FILE,
        account_hash,
        metadata,
        store,
        merge::merge_settings,
        |items| {
            SettingsEnvelope {
                v: ENVELOPE_V1,
                entries: items.to_vec(),
            }
            .to_bytes()
        },
        |bytes| SettingsEnvelope::from_bytes(bytes).map(|e| e.entries),
        |items| items.sort_by(|a, b| a.key.cmp(&b.key)),
        |i| i.updated_at,
    )
}

pub fn sync_stats_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = StatsItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    sync_domain(
        drive,
        STATS_FILE,
        account_hash,
        metadata,
        store,
        merge::merge_stats,
        |items| {
            StatsEnvelope {
                v: ENVELOPE_V1,
                entries: items.to_vec(),
            }
            .to_bytes()
        },
        |bytes| StatsEnvelope::from_bytes(bytes).map(|e| e.entries),
        |items| items.sort_by(|a, b| a.day.cmp(&b.day).then_with(|| a.event_id.cmp(&b.event_id))),
        |i| i.timestamp_ms,
    )
}

/// Run all four domains. Each domain is isolated: a failure in one does not
/// prevent the others from running; the first error is returned after all
/// domains have had their chance.
pub fn sync_all_domains(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    dict_store: &mut dyn DirtyStore<Item = DictionaryItem>,
    snippet_store: &mut dyn DirtyStore<Item = SnippetItem>,
    settings_store: &mut dyn DirtyStore<Item = SettingsItem>,
    stats_store: &mut dyn DirtyStore<Item = StatsItem>,
) -> Result<Vec<DomainSyncOutcome>, SyncError> {
    let mut outcomes = Vec::new();
    let mut first_error: Option<SyncError> = None;
    let steps: [(&str, Result<DomainSyncOutcome, SyncError>); 4] = [
        (
            "dictionary",
            sync_dictionary_domain(drive, account_hash, metadata, dict_store),
        ),
        (
            "snippets",
            sync_snippet_domain(drive, account_hash, metadata, snippet_store),
        ),
        (
            "stats",
            sync_stats_domain(drive, account_hash, metadata, stats_store),
        ),
        (
            "settings",
            sync_settings_domain(drive, account_hash, metadata, settings_store),
        ),
    ];
    for (label, result) in steps {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                log::warn!("sync: domain {label} failed: {e}");
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(outcomes),
    }
}
