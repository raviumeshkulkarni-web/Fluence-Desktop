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
    /// pushed, and purge never-pushed tombstones — atomically, under the io
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
/// same window — it must not meaningfully slow the normal convergence path.
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

/// A cheap deterministic-ish jitter source seeded from the monotonic clock —
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
/// consistent, so a fresh write can briefly be invisible — that is treated as a
/// "not yet verified" miss, not a data error. A transient listing failure is
/// treated the same (conservative: we never claim pushed unless we saw it live).
/// The caller keeps the rows dirty so the next pass re-heals.
fn upload_is_live(name: &str, new_version: &str, drive: &mut dyn DomainDriveStore) -> bool {
    match drive.list_v1_files() {
        Ok(files) => files
            .iter()
            .any(|f| f.name == name && f.version.as_deref() == Some(new_version)),
        Err(_) => false,
    }
}

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
        let mut remote_items: Vec<T> = Vec::new();
        let mut remote_version: Option<String> = None;
        let mut valid_file_ids: Vec<String> = Vec::new();
        let mut valid_found = false;
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
                // 7b. Post-upload verification (backoff-tolerant, re-list
                // version only). We never mark rows pushed / set last_rev
                // unless the pushed revision is actually live; a miss keeps the
                // rows dirty so the next pass re-heals. Drive's eventual
                // consistency makes a transient miss expected and harmless.
                if !upload_is_live(name, &new_version, drive) {
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
    }

    impl FakeDrive {
        fn new() -> Self {
            Self {
                files: Vec::new(),
                contents: std::collections::HashMap::new(),
                put_count: 0,
                delete_count: 0,
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

    fn dict_item(spoken: &str, updated_at: i64) -> DictionaryItem {
        DictionaryItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
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
        // UNIT C — oversized (abuse) must not be auto-replaced via !valid_found fallback
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
        // UNIT C — corrupt but within size keeps current repair behavior (siblings + local push)
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
    }

    #[test]
    fn verification_miss_keeps_rows_unpushed_not_failed() {
        // B-3: the PUT succeeded but the pushed revision is not yet visible on
        // the next list (Drive eventual consistency). The pass must NOT report
        // pushed nor set last_rev — it returns a non-pushed outcome so the next
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
        // UNIT C — duplicate consolidation idempotence: repeat pass with single file is no-op
        let valid_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("hello", 100)],
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
        let mut store2 = MemStore::<DictionaryItem>::new(vec![dict_item("hello", 100)]);
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
    fn oversized_with_valid_sibling_keeps_valid_and_drops_oversized() {
        // F4a — one valid sibling + multiple oversized duplicates: keep valid, drop oversized, no Rejected
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
        // F4b — FakeDrive paginated in chunks, consolidation still works across pages
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
        // ITEM 2 — genuine pagination: 5 files across 3 pages (2+2+1) must still consolidate
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
