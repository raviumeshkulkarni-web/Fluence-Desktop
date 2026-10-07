// Fluence sync - frozen v1.2 domain engine (dictionary, snippets, stats, settings)
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
// (treated as absent) - one bad domain never blocks the others.

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
/// the account's local set with the merged winners via `save_merged`.
///
/// `save_merged` performs the replace, the clean/pushed mark and the
/// never-pushed-tombstone purge in ONE transactional write per domain. A single
/// write is essential: a second load→mutate→write pass would stamp any local
/// edit made in between as pushed without it ever reaching the server, and that
/// silently-clean row would never be rescued again (it looks pushed).
pub trait DirtyStore {
    type Item: Clone + PartialEq;

    /// Rows owned by this account (live + tombstones).
    fn load(&self, account_hash: &str) -> Vec<Self::Item>;
    /// Stamp rows with no account ownership into this account (first login /
    /// enrollment). Returns how many rows were stamped.
    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError>;
    fn has_dirty(&self, account_hash: &str) -> bool;
    /// Replace this account's rows with the merged winners, mark them clean and
    /// pushed, and purge never-pushed tombstones - atomically, under the io
    /// lock.
    fn save_merged(&mut self, account_hash: &str, merged: Vec<Self::Item>)
        -> Result<(), SyncError>;
}

/// Maximum GET->MERGE->PUT cycles per domain per pass. Attempt 1 plus three
/// staleness retries; exhaustion surfaces as retryable so the scheduler
/// backs off and the next pass resumes from fresh state.
const MAX_ATTEMPTS: usize = 4;

/// Staleness-retry backoff bounds (ms). Small, because it only needs to break
/// a tight GET->PUT livelock when two devices invalidate each other in the
/// same window - it must not meaningfully slow the normal convergence path.
const STALE_RETRY_BASE_MS: u64 = 50;
const STALE_RETRY_MAX_MS: u64 = 600;

/// Jittered backoff (ms) to wait before a StaleVersion retry. `attempt` is the
/// 1-based attempt already consumed (so the first retry is attempt 2). Delay is
/// `base * 2^(attempt-2)`, capped, jittered into [0.5x, 1.5x] via `rand` (a
/// uniform-0..1 source). Attempt 1 performs no delay.
///
/// The jitter is the point: on a livelock both devices otherwise sleep the
/// exact same amount and keep colliding in lockstep; spreading the wait breaks
/// the thundering-herd and lets one device land a clean CAS.
fn stale_retry_delay_ms(attempt: usize, rand: &mut impl FnMut() -> f64) -> u64 {
    if attempt < 2 {
        return 0;
    }
    let exp = STALE_RETRY_BASE_MS.saturating_mul(1u64 << (attempt - 2).min(5));
    let base = exp.min(STALE_RETRY_MAX_MS);
    let jitter = base as f64 * (0.5 + rand() * 1.0);
    (jitter.round() as u64).min(STALE_RETRY_MAX_MS)
}

/// A cheap deterministic-ish jitter source seeded from the monotonic clock -
/// good enough to de-correlate contending devices without pulling in `rand`.
fn stale_retry_jitter_source() -> impl FnMut() -> f64 {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x536e61f700000001);
    let mut state = seed | 1;
    move || {
        // xorshift64*
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Post-upload verification: re-list the domain and confirm the revision the
/// PUT reported is actually the live remote version. Google Drive is eventually
/// consistent, so a fresh write can briefly be invisible - that is treated as a
/// "not yet verified" miss, not a data error. A transient listing failure is
/// treated the same (conservative: we never claim pushed unless we saw it live).
/// The caller keeps the rows dirty so the next pass re-heals.
fn upload_is_live(name: &str, new_version: &str, drive: &mut dyn DomainDriveStore) -> bool {
    upload_is_live_in(name, new_version, drive, &DomainScope::V1)
}

/// Where a domain file lives: the flat `v1` folder, or a per-account partition
/// subfolder (Phase 6 Agents/Styles). Threading the scope through the single
/// `sync_domain` loop — rather than duplicating its CAS/retry/verification
/// logic — is what keeps the partitioned path from diverging from the frozen
/// one.
#[derive(Clone, Copy)]
enum DomainScope<'a> {
    V1,
    Partition { folder_id: &'a str },
}

fn list_scope(
    drive: &mut dyn DomainDriveStore,
    scope: &DomainScope,
) -> Result<Vec<crate::sync::drive::DomainFileMeta>, SyncError> {
    match scope {
        DomainScope::V1 => drive.list_v1_files(),
        DomainScope::Partition { folder_id } => drive.list_partition_files(folder_id),
    }
}

fn put_scope(
    drive: &mut dyn DomainDriveStore,
    scope: &DomainScope,
    name: &str,
    content: &[u8],
    expected_version: Option<&str>,
    preferred_file_id: Option<&str>,
) -> Result<String, SyncError> {
    match scope {
        DomainScope::V1 => drive.put_domain(name, content, expected_version, preferred_file_id),
        DomainScope::Partition { folder_id } => drive.put_partitioned_domain(
            folder_id,
            name,
            content,
            expected_version,
            preferred_file_id,
        ),
    }
}

fn upload_is_live_in(
    name: &str,
    new_version: &str,
    drive: &mut dyn DomainDriveStore,
    scope: &DomainScope,
) -> bool {
    match list_scope(drive, scope) {
        Ok(files) => files
            .iter()
            .any(|f| f.name == name && f.version.as_deref() == Some(new_version)),
        Err(_) => false,
    }
}

/// One generic domain sync pass. All four domains differ only in their item
/// type, merge law, codec and ordering key - captured here as parameters.
fn sync_domain<T>(
    drive: &mut dyn DomainDriveStore,
    scope: DomainScope,
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
        let files = list_scope(drive, &scope)?;
        let mut domain_files: Vec<&DomainFileMeta> =
            files.iter().filter(|f| f.name == name).collect();
        domain_files.sort_by(|a, b| a.file_id.cmp(&b.file_id));
        let mut remote_items: Vec<T> = Vec::new();
        let mut remote_version: Option<String> = None;
        let mut valid_file_ids: Vec<String> = Vec::new();
        let mut valid_found = false;
        let mut unsupported_found = false;
        let mut oversized_found = false;
        let mut oversized_bytes: usize = 0;
        for meta in &domain_files {
            let bytes = match drive.get_domain_content(&meta.file_id)? {
                Some(b) => b,
                None => continue,
            };
            if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
                oversized_found = true;
                oversized_bytes = oversized_bytes.max(bytes.len());
                log::warn!(
                    "sync: {} file {} oversized {} bytes > {} cap, keeping remote intact",
                    name,
                    meta.file_id,
                    bytes.len(),
                    crate::sync::drive::MAX_DOMAIN_BYTES
                );
                continue;
            }
            if let Some(items) = decode(&bytes) {
                remote_items.extend(items);
                valid_file_ids.push(meta.file_id.clone());
                if remote_version.is_none() {
                    remote_version = meta.version.clone();
                }
                valid_found = true;
            } else if crate::sync::domain::is_future_envelope(&bytes) {
                // A newer client owns this file. It is NOT corrupt and must
                // never be repaired over; leave it completely untouched.
                unsupported_found = true;
            }
            // Corrupt envelope (within size): skip this file, keep its siblings.
        }
        // The update target and the version used for CAS must come from the
        // same valid file. Invalid/oversized siblings remain untouched.
        let preferred_file_id = valid_file_ids
            .first()
            .map(|id| id.as_str())
            .or_else(|| domain_files.first().map(|m| m.file_id.as_str()));
        // A corrupt-but-size-valid file is still a concrete remote revision.
        // Use that revision when repairing it so the repair is CAS-protected;
        // otherwise put_domain would reject every repair as stale forever.
        if remote_version.is_none() {
            remote_version = domain_files.iter().find_map(|m| m.version.clone());
        }
        // Oversized remote must not be auto-replaced via the !valid_found fallback (frozen.rs:135).
        // Keep remote intact, surface non-fatal Rejected instead of silently overwriting.
        // Only when no valid file exists and at least one oversized file exists do we surface; otherwise oversized is just an extra duplicate to ignore.

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
        // Distinguish oversized (abuse) from corrupt: oversized must not trigger the !valid_found fallback auto-replace (frozen.rs:135).
        if oversized_found && !valid_found {
            log::warn!(
                "sync: {} oversized remote ({} bytes > {}), keep remote intact, surface Rejected",
                name,
                oversized_bytes,
                crate::sync::drive::MAX_DOMAIN_BYTES
            );
            return Err(SyncError::Rejected(format!(
                "{}: remote file oversized {} bytes > {} cap, keeping remote intact",
                name,
                oversized_bytes,
                crate::sync::drive::MAX_DOMAIN_BYTES
            )));
        }
        // Only a future envelope remains: do not select a write target, and do
        // not let this be mistaken for "absent" (which would create a fresh file
        // and clobber the newer client's data). Independent of the dirty flag.
        if unsupported_found && !valid_found {
            return Ok(DomainSyncOutcome {
                pushed: false,
                merged: false,
                items_merged: 0,
            });
        }
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
        match put_scope(
            drive,
            &scope,
            name,
            &bytes,
            remote_version.as_deref(),
            preferred_file_id,
        ) {
            Ok(new_version) => {
                // 7b. Post-upload verification (backoff-tolerant, re-list
                // version only). We never mark rows pushed / set last_rev
                // unless the pushed revision is actually live; a miss keeps the
                // rows dirty so the next pass re-heals. Drive's eventual
                // consistency makes a transient miss expected and harmless.
                if !upload_is_live_in(name, &new_version, drive, &scope) {
                    log::debug!(
                        "sync: {name} upload {new_version} not visible yet; leaving rows dirty (verification miss)"
                    );
                    return Ok(DomainSyncOutcome {
                        pushed: false,
                        merged: false,
                        items_merged: merged.len(),
                    });
                }
                metadata.set_last_rev(account_hash, name, new_version);
                let count = merged.len();
                store.save_merged(account_hash, merged)?;
                // save_merged is a single transactional write: merge, clean
                // mark and tombstone purge happen together, so a local edit
                // landing concurrently can never be wrongly stamped as pushed.
                // Consolidate only valid duplicates whose contents were
                // included in the merged payload. Corrupt/oversized files
                // may contain unrecoverable data and must remain untouched.
                if let Some(target) = preferred_file_id {
                    for duplicate_id in valid_file_ids.iter().filter(|id| id.as_str() != target) {
                        let _ = drive.delete_domain_file(duplicate_id);
                    }
                }
                return Ok(DomainSyncOutcome {
                    pushed: true,
                    merged: outcome.changed,
                    items_merged: count,
                });
            }
            Err(SyncError::StaleVersion(live)) => {
                log::debug!(
                    "sync: {name} changed under us (live={live}); re-fetching (attempt {attempts})"
                );
                // Jittered backoff breaks the tight-loop livelock where two
                // devices invalidate each other's CAS in lockstep.
                if attempts >= 2 {
                    let delay = stale_retry_delay_ms(attempts, &mut stale_retry_jitter_source());
                    if delay > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(delay));
                    }
                }
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
        DomainScope::V1,
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
        DomainScope::V1,
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
        DomainScope::V1,
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
        DomainScope::V1,
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

// ── Phase 6: partitioned Agents/Styles domains ──────────────────────────────

/// Resolve the account partition folder id for this pass, or report "no
/// location" without touching the network.
///
/// `None` (unusable hash) is fail-closed at the transport boundary too, but
/// resolving here first avoids even the folder-listing call.
fn partition_folder_id(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
) -> Result<Option<String>, SyncError> {
    let Some(folder) = crate::sync::account_partition::folder_name(account_hash) else {
        return Ok(None);
    };
    Ok(Some(drive.ensure_partition_folder(&folder)?))
}

pub fn sync_agents_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = AgentItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    let Some(folder_id) = partition_folder_id(drive, account_hash)? else {
        // No usable identity: the domain is absent, not failed. Nothing is
        // read, nothing is written, and the outcome records no merge.
        return Ok(DomainSyncOutcome {
            pushed: false,
            merged: false,
            items_merged: 0,
        });
    };
    let scope = DomainScope::Partition {
        folder_id: &folder_id,
    };
    sync_domain(
        drive,
        scope,
        crate::sync::drive::AGENTS_FILE,
        account_hash,
        metadata,
        store,
        merge::merge_agents,
        |items| {
            AgentEnvelope {
                v: ENVELOPE_V1,
                entries: items.to_vec(),
            }
            .to_bytes()
        },
        |bytes| AgentEnvelope::from_bytes(bytes).map(|e| e.entries),
        |items| {
            items.sort_by(|a, b| {
                a.business_key
                    .cmp(&b.business_key)
                    .then_with(|| a.sync_id.cmp(&b.sync_id))
            })
        },
        |i| i.updated_at,
    )
}

pub fn sync_styles_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = StyleItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    let Some(folder_id) = partition_folder_id(drive, account_hash)? else {
        return Ok(DomainSyncOutcome {
            pushed: false,
            merged: false,
            items_merged: 0,
        });
    };
    let scope = DomainScope::Partition {
        folder_id: &folder_id,
    };
    sync_domain(
        drive,
        scope,
        crate::sync::drive::STYLES_FILE,
        account_hash,
        metadata,
        store,
        merge::merge_styles,
        |items| {
            StyleEnvelope {
                v: ENVELOPE_V1,
                entries: items.to_vec(),
            }
            .to_bytes()
        },
        |bytes| StyleEnvelope::from_bytes(bytes).map(|e| e.entries),
        |items| {
            items.sort_by(|a, b| {
                a.business_key
                    .cmp(&b.business_key)
                    .then_with(|| a.sync_id.cmp(&b.sync_id))
            })
        },
        |i| i.updated_at,
    )
}

/// Combined result of a multi-domain pass.
///
/// Per-domain outcomes are ALWAYS reported: a failing domain must not erase
/// the successful domains' writes and merges, which already landed by the time
/// any error is known. `error` carries the worst domain failure, if any, so
/// the scheduler escalates on the most actionable signal rather than on
/// whichever domain runs first.
pub struct AllDomainsResult {
    pub outcomes: Vec<DomainSyncOutcome>,
    pub error: Option<SyncError>,
}

/// Severity rank for cross-domain error selection. Higher wins.
///
/// Rationale (scheduler contract): stop-conditions outrank everything
/// (`Fatal` latches scheduling until re-armed; `AuthRequired`/`NotOurs` abort
/// the pass); permanent rejections outrank transients (a `Rejected` domain
/// will never heal by retrying, while a `Retryable` one might — reporting the
/// transient would either hot-loop without backoff or back off forever against
/// a permanently-broken sibling).
fn severity_rank(e: &SyncError) -> u8 {
    match e {
        SyncError::Fatal(_) => 5,
        SyncError::AuthRequired => 4,
        SyncError::NotOurs => 3,
        SyncError::Rejected(_) => 2,
        SyncError::Retryable(_) | SyncError::Throttled { .. } | SyncError::StaleVersion(_) => 1,
    }
}

/// Run all six domains. Each domain is isolated: a failure in one does not
/// prevent the others from running, and their outcomes are still reported.
/// The returned error (if any) is the worst-severity domain failure, not the
/// first in domain order.
pub fn sync_all_domains(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    dict_store: &mut dyn DirtyStore<Item = DictionaryItem>,
    snippet_store: &mut dyn DirtyStore<Item = SnippetItem>,
    settings_store: &mut dyn DirtyStore<Item = SettingsItem>,
    stats_store: &mut dyn DirtyStore<Item = StatsItem>,
    agent_store: &mut dyn DirtyStore<Item = AgentItem>,
    style_store: &mut dyn DirtyStore<Item = StyleItem>,
) -> AllDomainsResult {
    let mut outcomes = Vec::new();
    let mut worst_error: Option<SyncError> = None;
    let mut worst_rank = 0u8;
    let steps: [(&str, Result<DomainSyncOutcome, SyncError>); 6] = [
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
        (
            "agents",
            sync_agents_domain(drive, account_hash, metadata, agent_store),
        ),
        (
            "styles",
            sync_styles_domain(drive, account_hash, metadata, style_store),
        ),
    ];
    for (label, result) in steps {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                log::warn!("sync: domain {label} failed: {e}");
                let rank = severity_rank(&e);
                if rank > worst_rank {
                    worst_rank = rank;
                    worst_error = Some(e);
                }
            }
        }
    }
    AllDomainsResult {
        outcomes,
        error: worst_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::drive::{DomainFileMeta, MAX_DOMAIN_BYTES};
    use crate::sync::error::SyncError;
    use crate::sync::metadata::SyncMetadata;

    struct FakeDrive {
        files: Vec<DomainFileMeta>,
        contents: std::collections::HashMap<String, Vec<u8>>,
        put_count: usize,
        delete_count: usize,
        /// Partition folders, keyed by folder id: the per-account locations
        /// the partitioned domains read and write. Separate maps per folder id
        /// model what production guarantees via distinct Drive paths.
        partition_files: std::collections::HashMap<String, Vec<DomainFileMeta>>,
        partition_put_count: usize,
        canned_email: Option<String>,
    }

    impl FakeDrive {
        fn new() -> Self {
            Self {
                files: Vec::new(),
                contents: std::collections::HashMap::new(),
                put_count: 0,
                delete_count: 0,
                partition_files: std::collections::HashMap::new(),
                partition_put_count: 0,
                canned_email: Some("test@example.com".to_string()),
            }
        }
        fn with_file(mut self, name: &str, file_id: &str, version: &str, bytes: Vec<u8>) -> Self {
            self.files.push(DomainFileMeta {
                file_id: file_id.to_string(),
                name: name.to_string(),
                version: Some(version.to_string()),
            });
            self.contents.insert(file_id.to_string(), bytes);
            self
        }
    }

    impl crate::sync::drive::DomainDriveStore for FakeDrive {
        fn ensure_v1_folder(&mut self) -> Result<String, SyncError> {
            Ok("v1".to_string())
        }
        fn list_v1_files(&mut self) -> Result<Vec<DomainFileMeta>, SyncError> {
            Ok(self.files.clone())
        }
        fn get_domain_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
            Ok(self.contents.get(file_id).cloned())
        }
        fn put_domain(
            &mut self,
            name: &str,
            _content: &[u8],
            _expected_version: Option<&str>,
            preferred_file_id: Option<&str>,
        ) -> Result<String, SyncError> {
            self.put_count += 1;
            let new_version = format!("v{}", self.put_count + 10);
            let file_id = preferred_file_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| format!("id-created-{}", self.put_count));
            // Make the freshly written revision visible on the next list so
            // post-upload verification (upload_is_live) passes, like a real Drive.
            self.files.push(DomainFileMeta {
                file_id,
                name: name.to_string(),
                version: Some(new_version.clone()),
            });
            Ok(new_version)
        }
        fn delete_domain_file(&mut self, file_id: &str) -> Result<(), SyncError> {
            let _ = file_id;
            self.delete_count += 1;
            Ok(())
        }
        fn ensure_partition_folder(&mut self, folder_name: &str) -> Result<String, SyncError> {
            if !crate::sync::drive::is_partition_folder_name(folder_name) {
                return Err(SyncError::Rejected(format!(
                    "refusing to address non-partition folder {folder_name:?}"
                )));
            }
            Ok(format!("folder:{folder_name}"))
        }
        fn list_partition_files(
            &mut self,
            folder_id: &str,
        ) -> Result<Vec<DomainFileMeta>, SyncError> {
            Ok(self
                .partition_files
                .get(folder_id)
                .cloned()
                .unwrap_or_default())
        }
        fn put_partitioned_domain(
            &mut self,
            folder_id: &str,
            name: &str,
            content: &[u8],
            _expected_version: Option<&str>,
            preferred_file_id: Option<&str>,
        ) -> Result<String, SyncError> {
            self.partition_put_count += 1;
            let new_version = format!("pv{}", self.partition_put_count + 10);
            let file_id = preferred_file_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| format!("pid-created-{}", self.partition_put_count));
            self.contents.insert(file_id.clone(), content.to_vec());
            self.partition_files
                .entry(folder_id.to_string())
                .or_default()
                .push(DomainFileMeta {
                    file_id,
                    name: name.to_string(),
                    version: Some(new_version.clone()),
                });
            Ok(new_version)
        }
        fn drive_account_email(&mut self) -> Result<Option<String>, SyncError> {
            Ok(self.canned_email.clone())
        }
    }

    struct MemStore<T: Clone + PartialEq> {
        items: Vec<T>,
        has_dirty: bool,
    }

    impl<T: Clone + PartialEq> MemStore<T> {
        fn new(items: Vec<T>) -> Self {
            Self {
                items,
                has_dirty: false,
            }
        }
    }

    impl DirtyStore for MemStore<DictionaryItem> {
        type Item = DictionaryItem;
        fn load(&self, _h: &str) -> Vec<Self::Item> {
            self.items.clone()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.has_dirty
        }
        fn save_merged(&mut self, _h: &str, m: Vec<Self::Item>) -> Result<(), SyncError> {
            self.items = m;
            Ok(())
        }
    }
    impl DirtyStore for MemStore<SnippetItem> {
        type Item = SnippetItem;
        fn load(&self, _h: &str) -> Vec<Self::Item> {
            self.items.clone()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.has_dirty
        }
        fn save_merged(&mut self, _h: &str, m: Vec<Self::Item>) -> Result<(), SyncError> {
            self.items = m;
            Ok(())
        }
    }
    impl DirtyStore for MemStore<SettingsItem> {
        type Item = SettingsItem;
        fn load(&self, _h: &str) -> Vec<Self::Item> {
            self.items.clone()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.has_dirty
        }
        fn save_merged(&mut self, _h: &str, m: Vec<Self::Item>) -> Result<(), SyncError> {
            self.items = m;
            Ok(())
        }
    }
    impl DirtyStore for MemStore<StatsItem> {
        type Item = StatsItem;
        fn load(&self, _h: &str) -> Vec<Self::Item> {
            self.items.clone()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.has_dirty
        }
        fn save_merged(&mut self, _h: &str, m: Vec<Self::Item>) -> Result<(), SyncError> {
            self.items = m;
            Ok(())
        }
    }

    impl DirtyStore for MemStore<AgentItem> {
        type Item = AgentItem;
        fn load(&self, _h: &str) -> Vec<Self::Item> {
            self.items.clone()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.has_dirty
        }
        fn save_merged(&mut self, _h: &str, m: Vec<Self::Item>) -> Result<(), SyncError> {
            self.items = m;
            Ok(())
        }
    }

    impl DirtyStore for MemStore<StyleItem> {
        type Item = StyleItem;
        fn load(&self, _h: &str) -> Vec<Self::Item> {
            self.items.clone()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.has_dirty
        }
        fn save_merged(&mut self, _h: &str, m: Vec<Self::Item>) -> Result<(), SyncError> {
            self.items = m;
            Ok(())
        }
    }

    // ── STAGE 8C: cross-platform scenario simulation ────────────────────────
    //
    // These run the Windows engine against the EXACT bytes Android asserts in
    // `AgentStyleDomainSyncTest.agent_envelope_matches_the_cross_platform_fixture`
    // / `..._styles_...`. That is the strongest automated evidence of
    // Android↔Windows interoperability available short of two real devices: the
    // payload is literally what the other platform produces and consumes, not a
    // hand-built equivalent.

    /// The canonical account hash for `test@example.com` (STAGE 1 fixture).
    const CROSS_HASH: &str = "973dfe463ec85785f5f95af5ba3906eedb2d931c24e69824a89ea65dba4e813b";

    /// Byte-for-byte the Android agent fixture.
    const ANDROID_AGENT_BYTES: &str = "{\"v\":1,\"entries\":[{\"syncId\":\"123e4567-e89b-12d3-a456-426614174000\",\"businessKey\":\"agent:123e4567-e89b-12d3-a456-426614174001\",\"name\":\"Translator\",\"hint\":\"Be concise\",\"updatedAt\":1700000000000,\"deletedAt\":null,\"deviceId\":\"dev-a\"}]}\n";

    /// Byte-for-byte the Android style fixture.
    const ANDROID_STYLE_BYTES: &str = "{\"v\":1,\"entries\":[{\"syncId\":\"123e4567-e89b-12d3-a456-426614174000\",\"businessKey\":\"custom:123e4567-e89b-12d3-a456-426614174001\",\"name\":\"Formal\",\"hint\":\"Be formal\",\"updatedAt\":1700000000000,\"deletedAt\":null,\"deviceId\":\"dev-a\"}]}\n";

    fn cross_partition_folder() -> String {
        format!(
            "folder:{}",
            crate::sync::account_partition::folder_name(CROSS_HASH).unwrap()
        )
    }

    /// Android A → Windows A: Windows pulls the exact bytes Android uploaded and
    /// materializes the record into its own store.
    #[test]
    fn windows_pulls_an_android_authored_agent_partition() {
        let mut drive = FakeDrive::new();
        let folder = cross_partition_folder();
        drive.partition_files.insert(
            folder.clone(),
            vec![DomainFileMeta {
                file_id: "android-file".to_string(),
                name: crate::sync::drive::AGENTS_FILE.to_string(),
                version: Some("7".to_string()),
            }],
        );
        drive.contents.insert(
            "android-file".to_string(),
            ANDROID_AGENT_BYTES.as_bytes().to_vec(),
        );

        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<AgentItem>::new(vec![]);
        let out = sync_agents_domain(&mut drive, CROSS_HASH, &mut meta, &mut store)
            .expect("pull from an Android-authored partition must succeed");

        assert_eq!(1, store.items.len(), "the Android record must materialize");
        let got = &store.items[0];
        assert_eq!("Translator", got.name);
        assert_eq!("Be concise", got.hint);
        assert_eq!(
            "agent:123e4567-e89b-12d3-a456-426614174001",
            got.business_key
        );
        assert_eq!(1700000000000, got.updated_at);
        assert!(!out.pushed, "a converged pull must not rewrite the remote");
        assert_eq!(0, drive.partition_put_count);
    }

    /// Windows A → Android A: Windows's upload of the same record is
    /// byte-identical to the Android fixture, so an Android device parsing it
    /// sees exactly the same envelope.
    #[test]
    fn windows_uploads_agent_bytes_identical_to_the_android_fixture() {
        let mut drive = FakeDrive::new();
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<AgentItem>::new(vec![AgentItem {
            sync_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            business_key: "agent:123e4567-e89b-12d3-a456-426614174001".to_string(),
            name: "Translator".to_string(),
            hint: "Be concise".to_string(),
            updated_at: 1700000000000,
            deleted_at: None,
            device_id: "dev-a".to_string(),
        }]);
        store.has_dirty = true;

        sync_agents_domain(&mut drive, CROSS_HASH, &mut meta, &mut store)
            .expect("push to the partition must succeed");

        let folder = cross_partition_folder();
        let published = drive
            .partition_files
            .get(&folder)
            .and_then(|files| files.first())
            .map(|m| m.file_id.clone())
            .expect("a partition file must have been created");
        let bytes = drive.contents.get(&published).expect("content stored");
        assert_eq!(
            ANDROID_AGENT_BYTES.as_bytes(),
            bytes.as_slice(),
            "Windows upload must be byte-identical to the Android fixture"
        );
    }

    /// Style counterpart of the pull scenario (STAGE 8C item 15: styles must
    /// behave consistently with agents).
    #[test]
    fn windows_pulls_an_android_authored_style_partition() {
        let mut drive = FakeDrive::new();
        let folder = cross_partition_folder();
        drive.partition_files.insert(
            folder.clone(),
            vec![DomainFileMeta {
                file_id: "android-style-file".to_string(),
                name: crate::sync::drive::STYLES_FILE.to_string(),
                version: Some("9".to_string()),
            }],
        );
        drive.contents.insert(
            "android-style-file".to_string(),
            ANDROID_STYLE_BYTES.as_bytes().to_vec(),
        );

        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<StyleItem>::new(vec![]);
        sync_styles_domain(&mut drive, CROSS_HASH, &mut meta, &mut store)
            .expect("style pull must succeed");

        assert_eq!(1, store.items.len());
        assert_eq!("Formal", store.items[0].name);
        assert_eq!(
            "custom:123e4567-e89b-12d3-a456-426614174001",
            store.items[0].business_key
        );
    }

    /// Upload-before-verification (STAGE 8C item 12): an unusable account hash
    /// must produce an exactly zero-outcome pass with no partition addressed —
    /// no folder resolution, no listing, no write.
    #[test]
    fn agents_domain_fails_closed_without_a_usable_identity() {
        let mut drive = FakeDrive::new();
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<AgentItem>::new(vec![AgentItem {
            sync_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            business_key: "agent:123e4567-e89b-12d3-a456-426614174001".to_string(),
            name: "ShouldNeverUpload".to_string(),
            hint: "h".to_string(),
            updated_at: 1700000000000,
            deleted_at: None,
            device_id: "dev-a".to_string(),
        }]);
        store.has_dirty = true;

        for bad in ["", "not-a-hash", "0123456789abcdef", &"a".repeat(63)] {
            let out = sync_agents_domain(&mut drive, bad, &mut meta, &mut store)
                .expect("an unusable identity is absent, not failed");
            assert!(!out.pushed);
            assert_eq!(0, out.items_merged);
        }
        assert_eq!(
            0, drive.partition_put_count,
            "no partition write may occur without a usable identity"
        );
        assert!(
            drive.partition_files.is_empty(),
            "no partition folder may even be resolved"
        );
    }

    /// A foreign account's partition is a DIFFERENT folder id, so a pass for B
    /// cannot observe A's records (STAGE 8C items 1/6, automated analogue).
    #[test]
    fn a_partition_is_not_observable_by_a_different_account() {
        let mut drive = FakeDrive::new();
        let folder_a = format!(
            "folder:{}",
            crate::sync::account_partition::folder_name(CROSS_HASH).unwrap()
        );
        drive.partition_files.insert(
            folder_a,
            vec![DomainFileMeta {
                file_id: "a-file".to_string(),
                name: crate::sync::drive::AGENTS_FILE.to_string(),
                version: Some("1".to_string()),
            }],
        );
        drive.contents.insert(
            "a-file".to_string(),
            ANDROID_AGENT_BYTES.as_bytes().to_vec(),
        );

        let hash_b = crate::sync::metadata::account_hash_from_email("bob@example.com");
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<AgentItem>::new(vec![]);
        sync_agents_domain(&mut drive, &hash_b, &mut meta, &mut store)
            .expect("B's pass must succeed");

        assert!(store.items.is_empty(), "B must not observe A's agents");
        assert_ne!(
            crate::sync::account_partition::folder_name(CROSS_HASH).unwrap(),
            crate::sync::account_partition::folder_name(&hash_b).unwrap(),
            "the two accounts must not share a folder"
        );
    }

    fn dict_item(spoken: &str, updated_at: i64) -> DictionaryItem {
        dict_item_with_id(spoken, updated_at, &uuid::Uuid::new_v4().to_string())
    }

    /// Same as [`dict_item`] but with a caller-chosen `sync_id`, so a test can
    /// put the *same* record on both sides. `dict_item` randomises the id, which
    /// makes local and remote genuinely different records — fine for fixtures,
    /// wrong for asserting convergence.
    fn dict_item_with_id(spoken: &str, updated_at: i64, sync_id: &str) -> DictionaryItem {
        DictionaryItem {
            sync_id: sync_id.to_string(),
            spoken: spoken.to_string(),
            corrected: "fix".to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at,
            device_id: "dev-a".to_string(),
        }
    }

    #[test]
    fn oversized_remote_is_not_auto_replaced_keep_remote_intact() {
        // UNIT C - oversized (abuse) must not be auto-replaced via !valid_found fallback
        let oversized = vec![b'x'; MAX_DOMAIN_BYTES + 1];
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", oversized);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(
            matches!(res, Err(SyncError::Rejected(_))),
            "oversized must surface Rejected, not silently overwrite"
        );
        assert_eq!(drive.put_count, 0, "must not put over oversized remote");
    }

    #[test]
    fn corrupt_within_size_is_repaired_via_push() {
        // UNIT C - corrupt but within size keeps current repair behavior (siblings + local push)
        let corrupt = b"{ not json".to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", corrupt);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = false;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(
            res.is_ok(),
            "corrupt within size should be repaired, not rejected"
        );
        assert_eq!(
            drive.put_count, 1,
            "corrupt file should be repaired via push"
        );
    }

    #[test]
    fn future_envelope_version_is_never_overwritten() {
        // A remote written by a NEWER client must not be clobbered by this v1
        // client. Genuine corruption is still repaired (see
        // corrupt_within_size_is_repaired_via_push); an unsupported envelope
        // version is not corruption and must be left completely untouched.
        let future = br#"{"v":2,"entries":[]}"#.to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", future);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = false;
        let _ = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert_eq!(
            drive.put_count, 0,
            "a v2 remote must never be overwritten by a v1 client"
        );
    }

    #[test]
    fn future_envelope_is_never_overwritten_even_when_local_dirty() {
        // Companion to the above: with a dirty local row, has_dirty
        // independently forces a write. A fix that only refuses when clean
        // would still be destructive here.
        let future = br#"{"v":2,"entries":[]}"#.to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", future);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let _ = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert_eq!(
            drive.put_count, 0,
            "a dirty local must not force an overwrite of a v2 remote"
        );
    }

    #[test]
    fn future_envelope_with_entries_is_never_overwritten() {
        // A populated v2 payload is the case where a fix could plausibly skip
        // only empty unsupported payloads and still destroy real newer data.
        let future = br#"{"v":2,"entries":[{"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"newer","spoken":"newer","corrected":"newer","isEnabled":true,"updatedAt":9999,"deletedAt":null,"deviceId":"newer-device"}]}"#.to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", future);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        let _ = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert_eq!(
            drive.put_count, 0,
            "a populated v2 remote must never be overwritten by a v1 client"
        );
    }

    #[test]
    fn empty_remote_first_sync_creates_file() {
        // Normal first-sync: no remote file, local has data => must create
        let mut drive = FakeDrive::new();
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok());
        assert_eq!(drive.put_count, 1, "empty remote must create file");
    }

    struct LaggingFakeDrive {
        put_count: usize,
    }
    impl LaggingFakeDrive {
        fn new() -> Self {
            Self { put_count: 0 }
        }
    }
    impl crate::sync::drive::DomainDriveStore for LaggingFakeDrive {
        fn ensure_v1_folder(&mut self) -> Result<String, SyncError> {
            Ok("v1".to_string())
        }
        fn list_v1_files(&mut self) -> Result<Vec<DomainFileMeta>, SyncError> {
            // Never reflect a write (simulates Drive's eventual-consistency lag).
            Ok(Vec::new())
        }
        fn get_domain_content(&mut self, _file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
            Ok(None)
        }
        fn put_domain(
            &mut self,
            _name: &str,
            _content: &[u8],
            _expected_version: Option<&str>,
            _preferred_file_id: Option<&str>,
        ) -> Result<String, SyncError> {
            self.put_count += 1;
            Ok(format!("v{}", self.put_count + 100))
        }
        fn delete_domain_file(&mut self, _file_id: &str) -> Result<(), SyncError> {
            Ok(())
        }
        fn ensure_partition_folder(&mut self, folder_name: &str) -> Result<String, SyncError> {
            // Lagging semantics extend to partitions: the folder resolves, but
            // nothing written is ever reflected back.
            if !crate::sync::drive::is_partition_folder_name(folder_name) {
                return Err(SyncError::Rejected(format!(
                    "refusing to address non-partition folder {folder_name:?}"
                )));
            }
            Ok(format!("folder:{folder_name}"))
        }
        fn list_partition_files(
            &mut self,
            _folder_id: &str,
        ) -> Result<Vec<DomainFileMeta>, SyncError> {
            Ok(Vec::new())
        }
        fn put_partitioned_domain(
            &mut self,
            _folder_id: &str,
            _name: &str,
            _content: &[u8],
            _expected_version: Option<&str>,
            _preferred_file_id: Option<&str>,
        ) -> Result<String, SyncError> {
            self.put_count += 1;
            Ok(format!("v{}", self.put_count + 100))
        }
        fn drive_account_email(&mut self) -> Result<Option<String>, SyncError> {
            Ok(Some("test@example.com".to_string()))
        }
    }

    #[test]
    fn verification_miss_keeps_rows_unpushed_not_failed() {
        // B-3: the PUT succeeded but the pushed revision is not yet visible on
        // the next list (Drive eventual consistency). The pass must NOT report
        // pushed nor set last_rev - it returns a non-pushed outcome so the next
        // pass re-heals, instead of stamping a possibly-not-yet-live write as
        // permanently pushed.
        let mut drive = LaggingFakeDrive::new();
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "verification miss is not a hard error");
        let out = res.unwrap();
        assert_eq!(drive.put_count, 1, "the write happened");
        assert!(
            !out.pushed,
            "must not report pushed when revision is not yet live"
        );
        assert!(
            meta.get_last_rev("hash", DICT_FILE).is_none(),
            "must not set last_rev on a verification miss"
        );
    }

    #[test]
    fn duplicate_consolidation_idempotent() {
        // UNIT C - duplicate consolidation idempotence: repeat pass with single file is no-op
        //
        // "Already converged" requires local and remote to hold the SAME record,
        // so both sides are built from one shared sync_id. Using two separately
        // generated ids made them different records that tie on
        // (updatedAt, deviceId); the pass then legitimately has to push, which is
        // why this test only passed while ties were always resolved as "remote
        // wins" and became flaky once the order became total.
        let shared_id = "11111111-1111-4111-8111-111111111111";
        let shared_item = dict_item_with_id("hello", 100, shared_id);
        let valid_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![shared_item.clone()],
        }
        .to_bytes();
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-a", "1", valid_bytes.clone())
            .with_file("dictionary.json", "id-b", "2", valid_bytes.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(vec![]);
        let res1 = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res1.is_ok());
        assert!(
            drive.delete_count >= 1,
            "first pass should consolidate duplicates"
        );
        // Second pass: only one file remains, no dirty, already converged => no-op
        let mut drive2 = FakeDrive::new().with_file("dictionary.json", "id-a", "3", valid_bytes);
        let mut store2 = MemStore::<DictionaryItem>::new(vec![shared_item]);
        let res2 = sync_dictionary_domain(&mut drive2, "hash", &mut meta, &mut store2);
        assert!(res2.is_ok());
        assert_eq!(res2.unwrap().pushed, false, "repeat pass must be no-op");
        assert_eq!(
            drive2.delete_count, 0,
            "no extra deletes on idempotent repeat"
        );
        assert_eq!(drive2.put_count, 0, "no extra puts on idempotent repeat");
    }

    #[test]
    fn tie_where_local_wins_the_order_is_pushed_so_remote_converges() {
        // The mirror of the case above: local and remote genuinely differ
        // (same updatedAt/deviceId, different syncId). The order is total, so
        // exactly one side wins. When LOCAL wins the sync, the engine must push
        // so the remote converges, rather than silently discarding local.
        let local = dict_item_with_id("hello", 100, "ffffffff-ffff-4fff-8fff-ffffffffffff");
        let remote_item = dict_item_with_id("hello", 100, "00000000-0000-4000-8000-000000000001");
        let remote_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![remote_item],
        }
        .to_bytes();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-a", "1", remote_bytes);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(vec![local]);
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok());
        assert!(
            res.unwrap().pushed,
            "local wins the total order and must be pushed so the remote converges"
        );
        assert_eq!(drive.put_count, 1);
    }

    #[test]
    fn oversized_with_valid_sibling_keeps_valid_and_drops_oversized() {
        // F4a - one valid sibling + multiple oversized duplicates: keep valid, drop oversized, no Rejected
        let valid_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("hello", 100)],
        }
        .to_bytes();
        let oversized = vec![b'x'; MAX_DOMAIN_BYTES + 1];
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-valid", "1", valid_bytes.clone())
            .with_file("dictionary.json", "id-over1", "2", oversized.clone())
            .with_file("dictionary.json", "id-over2", "3", oversized.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(vec![]);
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(
            res.is_ok(),
            "valid sibling should prevent Rejected, oversized dups just ignored"
        );
        assert_eq!(
            drive.put_count, 0,
            "no push needed when valid remote already converged and no dirty"
        );
        // After successful pass, oversized dups are not deleted because no put happened (no consolidation)
        // But on next push with dirty, they would be cleaned. Here we just verify no Rejected and no overwrite.
    }

    #[test]
    fn paginated_list_still_consolidates() {
        // F4b - FakeDrive paginated in chunks, consolidation still works across pages
        // Simulate pagination by having list_v1_files return files that would have come from 2 pages
        let valid_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("hello", 100)],
        }
        .to_bytes();
        // Create 3 files as if they came from 2 pages (page_size=2)
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-a", "1", valid_bytes.clone())
            .with_file("dictionary.json", "id-b", "2", valid_bytes.clone())
            .with_file("dictionary.json", "id-c", "3", valid_bytes.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(vec![]);
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok());
        assert!(
            drive.delete_count >= 2,
            "should consolidate all 3 duplicates across simulated pages"
        );
    }

    #[test]
    fn genuine_pagination_page_size_2_consolidates_5_files() {
        // ITEM 2 - genuine pagination: 5 files across 3 pages (2+2+1) must still consolidate
        let valid_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("hello", 100)],
        }
        .to_bytes();
        struct PaginatedFakeDrive {
            files: Vec<DomainFileMeta>,
            contents: std::collections::HashMap<String, Vec<u8>>,
            put_count: usize,
            delete_count: usize,
            page_size: usize,
        }
        impl PaginatedFakeDrive {
            fn new(page_size: usize) -> Self {
                Self {
                    files: Vec::new(),
                    contents: std::collections::HashMap::new(),
                    put_count: 0,
                    delete_count: 0,
                    page_size,
                }
            }
            fn with_file(
                mut self,
                name: &str,
                file_id: &str,
                version: &str,
                bytes: Vec<u8>,
            ) -> Self {
                self.files.push(DomainFileMeta {
                    file_id: file_id.to_string(),
                    name: name.to_string(),
                    version: Some(version.to_string()),
                });
                self.contents.insert(file_id.to_string(), bytes);
                self
            }
        }
        impl crate::sync::drive::DomainDriveStore for PaginatedFakeDrive {
            fn ensure_v1_folder(&mut self) -> Result<String, SyncError> {
                Ok("v1".to_string())
            }
            fn list_v1_files(&mut self) -> Result<Vec<DomainFileMeta>, SyncError> {
                let mut all = Vec::new();
                let mut start = 0;
                while start < self.files.len() {
                    let end = (start + self.page_size).min(self.files.len());
                    all.extend(self.files[start..end].iter().cloned());
                    start = end;
                }
                Ok(all)
            }
            fn get_domain_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
                Ok(self.contents.get(file_id).cloned())
            }
            fn put_domain(
                &mut self,
                name: &str,
                _content: &[u8],
                _expected_version: Option<&str>,
                preferred_file_id: Option<&str>,
            ) -> Result<String, SyncError> {
                self.put_count += 1;
                let new_version = format!("v{}", self.put_count + 10);
                let file_id = preferred_file_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| format!("id-created-{}", self.put_count));
                // Reflect the write on the next list so post-upload verification passes.
                self.files.push(DomainFileMeta {
                    file_id,
                    name: name.to_string(),
                    version: Some(new_version.clone()),
                });
                Ok(new_version)
            }
            fn delete_domain_file(&mut self, file_id: &str) -> Result<(), SyncError> {
                let _ = file_id;
                self.delete_count += 1;
                Ok(())
            }
            fn ensure_partition_folder(&mut self, folder_name: &str) -> Result<String, SyncError> {
                if !crate::sync::drive::is_partition_folder_name(folder_name) {
                    return Err(SyncError::Rejected(format!(
                        "refusing to address non-partition folder {folder_name:?}"
                    )));
                }
                Ok(format!("folder:{folder_name}"))
            }
            fn list_partition_files(
                &mut self,
                _folder_id: &str,
            ) -> Result<Vec<DomainFileMeta>, SyncError> {
                Ok(Vec::new())
            }
            fn put_partitioned_domain(
                &mut self,
                _folder_id: &str,
                name: &str,
                _content: &[u8],
                _expected_version: Option<&str>,
                preferred_file_id: Option<&str>,
            ) -> Result<String, SyncError> {
                self.put_count += 1;
                let new_version = format!("v{}", self.put_count + 10);
                let file_id = preferred_file_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| format!("id-created-{}", self.put_count));
                self.files.push(DomainFileMeta {
                    file_id,
                    name: name.to_string(),
                    version: Some(new_version.clone()),
                });
                Ok(new_version)
            }
            fn drive_account_email(&mut self) -> Result<Option<String>, SyncError> {
                Ok(Some("test@example.com".to_string()))
            }
        }
        let mut drive = PaginatedFakeDrive::new(2)
            .with_file("dictionary.json", "id-a", "1", valid_bytes.clone())
            .with_file("dictionary.json", "id-b", "2", valid_bytes.clone())
            .with_file("dictionary.json", "id-c", "3", valid_bytes.clone())
            .with_file("dictionary.json", "id-d", "4", valid_bytes.clone())
            .with_file("dictionary.json", "id-e", "5", valid_bytes.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(vec![]);
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok());
        assert!(
            drive.delete_count >= 4,
            "should consolidate all 5 duplicates across genuine paginated pages"
        );
    }

    #[test]
    fn stale_retry_delay_grows_and_jitters_within_bounds() {
        // Attempt 1 (no retry yet) must never sleep.
        assert_eq!(stale_retry_delay_ms(1, &mut || 0.0), 0);
        assert_eq!(stale_retry_delay_ms(1, &mut || 1.0), 0);

        // Attempt 2 -> base 50ms, jittered into [25, 75] for rand in [0,1).
        assert_eq!(stale_retry_delay_ms(2, &mut || 0.0), 25);
        assert_eq!(stale_retry_delay_ms(2, &mut || 1.0), 75);

        // Attempt 3 -> 100ms base -> [50, 150].
        assert_eq!(stale_retry_delay_ms(3, &mut || 0.0), 50);
        assert_eq!(stale_retry_delay_ms(3, &mut || 1.0), 150);

        // Attempt 4 -> 200ms base -> [100, 300].
        assert_eq!(stale_retry_delay_ms(4, &mut || 0.0), 100);
        assert_eq!(stale_retry_delay_ms(4, &mut || 1.0), 300);

        // The cap must hold no matter how many attempts accumulate.
        let big = stale_retry_delay_ms(20, &mut || 1.0);
        assert!(
            big <= STALE_RETRY_MAX_MS,
            "delay never exceeds the cap (got {big})"
        );
        for a in 2..=8 {
            assert!(stale_retry_delay_ms(a, &mut || 1.0) <= STALE_RETRY_MAX_MS);
            assert!(
                stale_retry_delay_ms(a, &mut || 0.0) >= stale_retry_delay_ms(a, &mut || 1.0) / 3
            );
        }

        // A real jitter source stays in range and is not constant (de-correlates).
        let mut src = stale_retry_jitter_source();
        let mut prev = src();
        let mut varies = false;
        for _ in 0..20 {
            let v = src();
            assert!((0.0..1.0).contains(&v));
            if (v - prev).abs() > 1e-9 {
                varies = true;
            }
            prev = v;
        }
        assert!(varies, "jitter source must produce varying values");
    }
}
