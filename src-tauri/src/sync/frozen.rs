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
    /// Some remote state was skipped as unusable (oversized/corrupt/future)
    /// while the rest of the pass completed. The skipped files are retained
    /// untouched; the flag only reports that convergence is partial.
    /// Currently set by the sharded Stats engine; other domains report
    /// `false` (their frozen paths are unchanged).
    pub degraded: bool,
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

/// F5: the listing carries each file's byte size — when it already reports
/// over-cap, skip the content download entirely instead of re-downloading an
/// oversized file every pass only to discard it. Returns the listed size when
/// skipping. Unknown sizes (`None`) still download and check as before, so a
/// listing that omits `size` changes nothing.
fn listing_size_over_cap(meta: &DomainFileMeta) -> Option<u64> {
    meta.size
        .filter(|&s| s > crate::sync::drive::MAX_DOMAIN_BYTES as u64)
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
            // F5: listing-reported oversize skips the download, not just the
            // decode — the bytes would only be discarded.
            if let Some(listed) = listing_size_over_cap(meta) {
                oversized_found = true;
                oversized_bytes = oversized_bytes.max(listed as usize);
                log::warn!(
                    "sync: {} file {} listed {} bytes > {} cap, skipping download, keeping remote intact",
                    name,
                    meta.file_id,
                    listed,
                    crate::sync::drive::MAX_DOMAIN_BYTES
                );
                continue;
            }
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
                let usable = !items.is_empty();
                remote_items.extend(items);
                // FENCE (#7): per-record isolation means an envelope whose
                // every record is structurally malformed now decodes to an
                // EMPTY list instead of `None`. That is still a readable file,
                // so it is never repaired over — but it must NOT be recorded
                // as a VALID duplicate. We cannot tell "the user emptied this
                // file" apart from "every record in it was garbage this client
                // could not parse", and consolidation deletes valid duplicates.
                // Registering it would let a pass DELETE a file whose real
                // contents we simply failed to read. Readable-but-empty files
                // therefore contribute records, but are never a write target
                // and never shed.
                if usable {
                    valid_file_ids.push(meta.file_id.clone());
                    if remote_version.is_none() {
                        remote_version = meta.version.clone();
                    }
                    valid_found = true;
                }
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
                degraded: false,
            });
        }
        let needs_push = state_differs
            || store.has_dirty(account_hash)
            || (!valid_found && !merged.is_empty())
            // Duplicate valid files must force the PUT, not merely skip the pass.
            // The consolidation that actually deletes them lives INSIDE the
            // `Ok(..)` arm of the write below, so a pass that short-circuits at
            // `!needs_push` never reaches it. With two readable same-name files
            // holding disjoint records, `remote_items` is their union, so
            // `state_differs` is false and neither other term fires — the
            // duplicates were re-listed and re-downloaded every cadence, forever,
            // never merged onto one file. Android forces the same write through
            // `|| fetch.hasDuplicateValidFiles`; this is its missing twin.
            //
            // Known cost: if `delete_domain_file` keeps failing (permissions, a
            // transient Drive error), every later pass now performs a full PUT
            // plus a doomed delete where it previously did two idle reads. It is
            // bounded per pass by `MAX_ATTEMPTS` and still strictly better than
            // never consolidating at all.
            || valid_file_ids.len() > 1;

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
                    degraded: false,
                });
            }
            return Ok(DomainSyncOutcome {
                pushed: false,
                merged: false,
                items_merged: merged.len(),
                degraded: false,
            });
        }

        // 5. Never upload state we cannot re-parse (roundtrip validation).
        let bytes = encode(&merged);
        // SIZE check first, and separately. `decode` rejects anything over
        // MAX_DOMAIN_BYTES by returning `None`, so an oversized-but-otherwise-
        // valid merged set failed this gate and reported a factually wrong
        // message ("failed roundtrip validation") — and, worse, as `Fatal`,
        // which LATCHES scheduling until the user manually re-arms sync. A
        // dictionary that simply grew past 8 MiB is a size condition, not a
        // code bug, and it should degrade rather than stop sync dead. Android's
        // twin already reports this accurately as `Rejected`.
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return Err(SyncError::Rejected(format!(
                "{name}: merged state serializes to {} bytes, over the {} byte cap; \
                 upload refused and remote left intact",
                bytes.len(),
                crate::sync::drive::MAX_DOMAIN_BYTES
            )));
        }
        if decode(&bytes) != Some(merged.clone()) {
            return Err(SyncError::Fatal(format!(
                // F5 actionability: this fires only on bytes WE encoded (remote
                // content never reaches it — undecodable remotes are skipped,
                // oversized ones Rejected above). Local data is intact and
                // nothing was uploaded; the remedy is a code fix (app update),
                // not a retry — hence Fatal rather than a retry loop.
                "{name}: serialized envelope failed roundtrip validation; upload refused, local data intact"
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
                        degraded: false,
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
                    degraded: false,
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

// ── Canonical write order (frozen README) ───────────────────────────────────
//
// `to_bytes` does NOT sort — the deterministic sort is the caller's
// obligation, and these are the production sorters used before every write.
// They are named functions (not inline closures) so the write-side ordering
// tests can exercise the real comparator instead of a copy of it (audit #21).

/// dict/snippet: businessKey, then syncId.
pub(crate) fn sort_dictionary_items(items: &mut [DictionaryItem]) {
    items.sort_by(|a, b| {
        a.business_key()
            .cmp(&b.business_key())
            .then_with(|| a.sync_id.cmp(&b.sync_id))
    });
}

pub(crate) fn sort_snippet_items(items: &mut [SnippetItem]) {
    items.sort_by(|a, b| {
        a.business_key()
            .cmp(&b.business_key())
            .then_with(|| a.sync_id.cmp(&b.sync_id))
    });
}

/// settings: key.
pub(crate) fn sort_settings_items(items: &mut [SettingsItem]) {
    items.sort_by(|a, b| a.key.cmp(&b.key));
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
        sort_dictionary_items,
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
        sort_snippet_items,
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
        sort_settings_items,
        |i| i.updated_at,
    )
}

pub fn sync_stats_domain(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = StatsItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    sync_stats_sharded(drive, account_hash, metadata, store)
}

/// F2: LWW time of a merged stats set for the `max_seen` floor. Uses
/// `updated_at` (missing → 0), byte-identical in spirit to Android's
/// `merged.maxOfOrNull { it.updatedAt } ?: 0L`. Event time (`timestamp_ms`)
/// must never feed the floor: a re-merged old dictation would otherwise
/// inflate it, and the two platforms would stamp from different floors after
/// the same merge. `update_max_seen` only ever raises, so an all-unstamped
/// merge (floor 0) is a no-op rather than a regression.
fn stats_floor_ts(merged: &[StatsItem]) -> i64 {
    merged
        .iter()
        .map(|i| i.updated_at.unwrap_or(0))
        .max()
        .unwrap_or(0)
}

// ── Stats time shards ───────────────────────────────────────────────────────
// The Stats ledger is one LOGICAL set of per-dictation events stored as
// SEVERAL physical v1 envelopes: the frozen legacy `stats.json` plus monthly
// shards `stats-YYYY-MM.json` (overflow `stats-YYYY-MM-pNN.json`).
// See `stats_shards.rs` for naming, chunking, and the shard-content rule.
//
// Per-file guarantees mirror `sync_domain` exactly: CAS PUT per file,
// upload-liveness re-list per file, stale-version full re-merge, corrupt /
// oversized / future siblings skipped and retained (never auto-repaired,
// never deleted). Shard files are never deleted; same-name valid duplicates
// are consolidated onto the preferred target after a live PUT, like the
// flat domains. `stats.json` is NEVER written and NEVER deleted by this
// path: old released clients own it, new clients only union it. Fresh
// accounts may have NO legacy file at all: new clients simply sync shards,
// and if an old client later creates `stats.json` (its create-on-absent
// path) the next pass unions it in with no rewrite. Seeding legacy from new
// clients was rejected: it would duplicate history or route growth back
// into the uncapped file.
//
// No cross-file atomicity is assumed: each chunk PUT is independent, and a
// partially-live pass simply leaves rows dirty so the next pass re-heals the
// remainder (unchanged chunks compare equal and are skipped).
//
// Push decisions are diff-driven (desired chunk bytes vs remote), NOT
// `has_dirty`-driven: dirty flags are sticky under the mid-pass LWW
// tie-guard (an uploaded row ties with itself), so `has_dirty` is
// permanently true in steady state and must never gate PUTs.
fn sync_stats_sharded(
    drive: &mut dyn DomainDriveStore,
    account_hash: &str,
    metadata: &mut SyncMetadata,
    store: &mut dyn DirtyStore<Item = StatsItem>,
) -> Result<DomainSyncOutcome, SyncError> {
    use crate::sync::stats_shards as shards;

    // 1. Enrollment: claim unstamped local rows into this account first.
    store.stamp_account(account_hash)?;
    let local_items = store.load(account_hash);

    let mut attempts = 0usize;
    while attempts < MAX_ATTEMPTS {
        attempts += 1;

        // 2. Read remote state: legacy (read-only) + every shard file.
        let files = drive.list_v1_files()?;
        let mut legacy_valid: Vec<StatsItem> = Vec::new();
        let mut legacy_unusable = false;
        for meta in files.iter().filter(|f| f.name == STATS_FILE) {
            // F5: listing-reported oversize skips the download (legacy is
            // read-only anyway; the bytes would only be discarded).
            if let Some(listed) = listing_size_over_cap(meta) {
                log::warn!(
                    "sync: stats legacy file {} listed {} bytes > {} cap, skipping download, keeping intact",
                    meta.file_id,
                    listed,
                    crate::sync::drive::MAX_DOMAIN_BYTES
                );
                legacy_unusable = true;
                continue;
            }
            let bytes = match drive.get_domain_content(&meta.file_id)? {
                Some(b) => b,
                None => continue,
            };
            if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
                log::warn!(
                    "sync: stats legacy file {} oversized {} bytes > {} cap, keeping intact",
                    meta.file_id,
                    bytes.len(),
                    crate::sync::drive::MAX_DOMAIN_BYTES
                );
                legacy_unusable = true;
                continue;
            }
            match StatsEnvelope::from_bytes(&bytes) {
                Some(env) => legacy_valid.extend(env.entries),
                None if crate::sync::domain::is_future_envelope(&bytes) => {
                    // A newer client owns the legacy file. Not corrupt, never
                    // repaired, never written; shards still sync around it.
                    legacy_unusable = true;
                }
                None => {
                    // Corrupt legacy: treated as absent for merging. Frozen
                    // means frozen: no repair PUT is ever attempted.
                    log::warn!(
                        "sync: stats legacy file {} corrupt, reading shards only",
                        meta.file_id
                    );
                    legacy_unusable = true;
                }
            }
        }

        // Shard files grouped by exact file name (same-name duplicates merge).
        let mut shard_names: Vec<String> = files
            .iter()
            .filter(|f| shards::is_stats_shard_file_name(&f.name))
            .map(|f| f.name.clone())
            .collect();
        shard_names.sort();
        shard_names.dedup();
        // Per-name remote state.
        struct RemoteShard {
            valid_entries: Vec<StatsItem>,
            valid_ids: Vec<String>,
            preferred: Option<DomainFileMeta>,
            repair_target: Option<DomainFileMeta>,
        }
        let mut remote_by_name: std::collections::HashMap<String, RemoteShard> =
            std::collections::HashMap::new();
        let mut any_shard_skipped = false;
        for name in &shard_names {
            let mut metas: Vec<&DomainFileMeta> =
                files.iter().filter(|f| &f.name == name).collect();
            metas.sort_by(|a, b| a.file_id.cmp(&b.file_id));
            let mut valid_entries = Vec::new();
            let mut valid_ids = Vec::new();
            let mut preferred: Option<DomainFileMeta> = None;
            let mut repair_target: Option<DomainFileMeta> = None;
            let mut skipped = false;
            for meta in metas {
                // F5: listing-reported oversize skips the download, not just
                // the decode — the bytes would only be discarded.
                if let Some(listed) = listing_size_over_cap(meta) {
                    log::warn!(
                        "sync: stats shard {name} file {} listed {} bytes > {} cap, skipping download, keeping intact",
                        meta.file_id,
                        listed,
                        crate::sync::drive::MAX_DOMAIN_BYTES
                    );
                    skipped = true;
                    continue;
                }
                let bytes = match drive.get_domain_content(&meta.file_id)? {
                    Some(b) => b,
                    None => continue,
                };
                if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
                    log::warn!(
                        "sync: stats shard {name} file {} oversized, keeping intact",
                        meta.file_id
                    );
                    skipped = true;
                    continue;
                }
                match StatsEnvelope::from_bytes(&bytes) {
                    Some(env) => {
                        // FENCE (#7), same rule as `sync_domain`: per-record
                        // isolation means a shard whose every record is
                        // malformed decodes to an EMPTY envelope rather than
                        // `None`. It must not be registered as a valid
                        // duplicate — consolidation deletes valid duplicates,
                        // and we cannot tell an intentionally-empty shard from
                        // one we simply failed to parse. It is not a write
                        // target either, so the CAS-protected repair path below
                        // is the only thing that may ever overwrite it.
                        let usable = !env.entries.is_empty();
                        valid_entries.extend(env.entries);
                        if usable {
                            valid_ids.push(meta.file_id.clone());
                            if preferred.is_none() {
                                preferred = Some((*meta).clone());
                            }
                        } else if repair_target.is_none() {
                            repair_target = Some((*meta).clone());
                        }
                    }
                    None if crate::sync::domain::is_future_envelope(&bytes) => {
                        skipped = true;
                    }
                    None => {
                        // Corrupt shard: kept for a CAS-protected repair only
                        // when this pass has content for the name; never
                        // deleted, never treated as empty data.
                        if repair_target.is_none() {
                            repair_target = Some((*meta).clone());
                        }
                    }
                }
            }
            // Same-name valid duplicates merge under the union (deduped below).
            if skipped {
                any_shard_skipped = true;
            }
            remote_by_name.insert(
                name.clone(),
                RemoteShard {
                    valid_entries,
                    valid_ids,
                    preferred,
                    repair_target,
                },
            );
        }

        // 3. Deterministic global merge: local, legacy, then shards by name.
        // Local wins every LWW tie, preserving two-list behavior exactly.
        let mut ordered: Vec<Vec<StatsItem>> = Vec::with_capacity(2 + remote_by_name.len());
        ordered.push(local_items.clone());
        ordered.push(legacy_valid.clone());
        for name in &shard_names {
            ordered.push(remote_by_name[name].valid_entries.clone());
        }
        let mut merged = shards::union_stats_merge(&ordered);
        shards::sort_stats_records(&mut merged);

        // 4. Desired shard content per month: merged rows that are NOT
        // byte-identical to the legacy version (history is never recopied).
        let legacy_by_id = shards::index_by_event_id(&legacy_valid);
        let mut months: Vec<String> = shards::months_in(&merged);
        for name in &shard_names {
            if let Some((month, _)) = shards::shard_sort_key(name) {
                if !months.contains(&month) {
                    months.push(month);
                }
            }
        }
        months.sort();

        // Each desired chunk: (file name, rows, expected_version, preferred).
        struct PendingPut {
            name: String,
            bytes: Vec<u8>,
            expected_version: Option<String>,
            preferred_file_id: Option<String>,
            extra_valid_ids: Vec<String>,
        }
        let mut pending: Vec<PendingPut> = Vec::new();
        for month in &months {
            let desired = shards::desired_shard_rows(month, &merged, &legacy_by_id);
            let chunks = shards::chunk_month_records(month, desired)?;
            if chunks.is_empty() {
                // Nothing desired for this month: remote files (stale losers
                // or corrupt) are left permanently intact, never rewritten
                // to empty and never deleted.
                continue;
            }
            for (name, rows) in chunks {
                let remote = remote_by_name.get(&name);
                let mut remote_sorted = remote.map(|r| r.valid_entries.clone()).unwrap_or_default();
                shards::sort_stats_records(&mut remote_sorted);
                let mut want_sorted = rows.clone();
                shards::sort_stats_records(&mut want_sorted);
                let dupes = remote.map(|r| r.valid_ids.len() > 1).unwrap_or(false);
                if remote_sorted == want_sorted && !dupes {
                    continue;
                }
                let bytes = StatsEnvelope {
                    v: ENVELOPE_V1,
                    entries: want_sorted.clone(),
                }
                .to_bytes();
                // Never upload state we cannot re-parse (roundtrip gate).
                match StatsEnvelope::from_bytes(&bytes) {
                    Some(rt) if rt.entries == want_sorted => {}
                    _ => {
                        return Err(SyncError::Fatal(format!(
                            // F5 actionability: fires only on bytes WE encoded;
                            // local data is intact and nothing was uploaded.
                            "stats: shard {name} failed roundtrip validation; upload refused, local data intact"
                        )));
                    }
                }
                let (expected_version, preferred_file_id) = match remote {
                    Some(r) if r.preferred.is_some() => (
                        r.preferred.as_ref().unwrap().version.clone(),
                        r.preferred.as_ref().unwrap().file_id.clone().into(),
                    ),
                    Some(r) if r.repair_target.is_some() => (
                        r.repair_target.as_ref().unwrap().version.clone(),
                        r.repair_target.as_ref().unwrap().file_id.clone().into(),
                    ),
                    _ => (None, None),
                };
                let extra_valid_ids = remote
                    .map(|r| {
                        let pref = r.preferred.as_ref().map(|p| p.file_id.clone());
                        r.valid_ids
                            .iter()
                            .filter(|id| Some(*id) != pref.as_ref())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                pending.push(PendingPut {
                    name,
                    bytes,
                    expected_version,
                    preferred_file_id,
                    extra_valid_ids,
                });
            }
        }

        if pending.is_empty() {
            // Converged (or nothing to write): materialize pulled rows.
            let local_set: std::collections::HashSet<StatsItem> =
                local_items.iter().cloned().collect();
            let merged_set: std::collections::HashSet<StatsItem> = merged.iter().cloned().collect();
            if local_set != merged_set {
                let max_ts = stats_floor_ts(&merged);
                let _io = crate::sync::io_lock::io_lock_guard();
                metadata.update_max_seen(account_hash, max_ts);
                let count = merged.len();
                store.save_merged(account_hash, merged)?;
                return Ok(DomainSyncOutcome {
                    pushed: false,
                    merged: true,
                    items_merged: count,
                    degraded: legacy_unusable || any_shard_skipped,
                });
            }
            return Ok(DomainSyncOutcome {
                pushed: false,
                merged: false,
                items_merged: merged.len(),
                degraded: legacy_unusable || any_shard_skipped,
            });
        }

        // 5. Advance the monotonic clock floor before writing.
        let _io = crate::sync::io_lock::io_lock_guard();
        let max_ts = stats_floor_ts(&merged);
        metadata.update_max_seen(account_hash, max_ts);
        drop(_io);

        // 6. Version-checked upload per chunk. A liveness miss returns
        // immediately (rows stay dirty; the next pass re-heals), exactly like
        // the flat domains. A stale version retries the whole ledger inside
        // this pass (idempotent: live chunks compare equal next attempt).
        let scope = DomainScope::V1;
        let mut stale_retry = false;
        for put in &pending {
            match put_scope(
                drive,
                &scope,
                &put.name,
                &put.bytes,
                put.expected_version.as_deref(),
                put.preferred_file_id.as_deref(),
            ) {
                Ok(new_version) => {
                    if !upload_is_live_in(&put.name, &new_version, drive, &scope) {
                        log::debug!(
                            "sync: stats shard {} upload {new_version} not visible yet; leaving rows dirty",
                            put.name
                        );
                        return Ok(DomainSyncOutcome {
                            pushed: false,
                            merged: false,
                            items_merged: merged.len(),
                            degraded: legacy_unusable || any_shard_skipped,
                        });
                    }
                    metadata.set_last_rev(account_hash, &put.name, new_version);
                    // Consolidate only valid same-name duplicates included in
                    // the merged payload. Corrupt/oversized files stay intact.
                    if let Some(target) = put.preferred_file_id.as_deref() {
                        for duplicate_id in &put.extra_valid_ids {
                            if duplicate_id != target {
                                let _ = drive.delete_domain_file(duplicate_id);
                            }
                        }
                    }
                }
                Err(SyncError::StaleVersion(live)) => {
                    log::debug!(
                        "sync: stats shard {} changed under us (live={live}); re-fetching",
                        put.name
                    );
                    if attempts >= 2 {
                        let delay =
                            stale_retry_delay_ms(attempts, &mut stale_retry_jitter_source());
                        if delay > 0 {
                            std::thread::sleep(std::time::Duration::from_millis(delay));
                        }
                    }
                    stale_retry = true;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        if stale_retry {
            continue;
        }
        let count = merged.len();
        store.save_merged(account_hash, merged)?;
        return Ok(DomainSyncOutcome {
            pushed: true,
            merged: true,
            items_merged: count,
            degraded: legacy_unusable || any_shard_skipped,
        });
    }
    Err(SyncError::Retryable(format!(
        "stats: state kept changing during sync ({MAX_ATTEMPTS} attempts)"
    )))
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
            degraded: false,
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
            degraded: false,
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
    use crate::sync::stats_shards as shards;

    struct FakeDrive {
        files: Vec<DomainFileMeta>,
        contents: std::collections::HashMap<String, Vec<u8>>,
        put_count: usize,
        delete_count: usize,
        /// Content downloads performed (F5: listing-size short-circuits must
        /// avoid re-downloading known-oversized files every pass).
        get_count: usize,
        /// Partition folders, keyed by folder id: the per-account locations
        /// the partitioned domains read and write. Separate maps per folder id
        /// model what production guarantees via distinct Drive paths.
        partition_files: std::collections::HashMap<String, Vec<DomainFileMeta>>,
        partition_put_count: usize,
        canned_email: Option<String>,
        /// Test-only hook fired from inside a partitioned upload, i.e. while a
        /// pass is between its local snapshot and its merge write-back. Used to
        /// reproduce a concurrent local mutation deterministically instead of
        /// racing two threads (audit finding H5).
        on_partitioned_put: Option<Box<dyn FnMut()>>,
        /// Test-only: make every partitioned upload fail, to exercise the path
        /// where the pass aborts and must therefore leave local state alone.
        partition_put_error: Option<SyncError>,
        /// Test-only: payload of the last V1 upload, so a test can assert WHAT
        /// was written — the destructive half of audit #7 is a push whose
        /// payload silently omits records the remote still had.
        last_put: Option<Vec<u8>>,
    }

    impl FakeDrive {
        fn new() -> Self {
            Self {
                files: Vec::new(),
                contents: std::collections::HashMap::new(),
                put_count: 0,
                delete_count: 0,
                get_count: 0,
                partition_files: std::collections::HashMap::new(),
                partition_put_count: 0,
                canned_email: Some("test@example.com".to_string()),
                on_partitioned_put: None,
                partition_put_error: None,
                last_put: None,
            }
        }
        fn with_file(mut self, name: &str, file_id: &str, version: &str, bytes: Vec<u8>) -> Self {
            // A real listing reports the size — the fake models that so
            // listing-size short-circuits are exercised, not bypassed.
            let size = Some(bytes.len() as u64);
            self.files.push(DomainFileMeta {
                file_id: file_id.to_string(),
                name: name.to_string(),
                version: Some(version.to_string()),
                size,
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
            self.get_count += 1;
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
            self.last_put = Some(_content.to_vec());
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
                size: Some(_content.len() as u64),
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
            if let Some(err) = &self.partition_put_error {
                return Err(err.clone());
            }
            // Mid-pass window: everything the pass snapshotted is already
            // encoded, so this is exactly when a user's edit would land.
            if let Some(hook) = self.on_partitioned_put.as_mut() {
                hook();
            }
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
                    size: Some(content.len() as u64),
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
                size: None,
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
                size: None,
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
                size: None,
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

    // ── Audit finding H5: concurrent local mutations during an in-flight pass ──
    //
    // `sync_agents_domain` snapshots the local store BEFORE the Drive round
    // trip and writes the merge result back AFTER it. These tests reproduce
    // that window deterministically: the FakeDrive hook fires inside the
    // partitioned upload, i.e. after the snapshot has been encoded and before
    // `save_merged` runs. The real `AgentDirtyStore` is used, so the actual
    // on-disk reconcile is exercised - not an in-memory stand-in.
    //
    // Pre-fix, `save_merged` wholesale-wrote the stale snapshot, so a create
    // vanished with no copy anywhere, and an edit/delete was silently reverted.

    /// A valid agent business key. The engine's round-trip gate validates
    /// `agent:<uuid>`, so a fixture with a free-form id would be rejected as
    /// corrupt and would mask the behaviour under test.
    fn agent_id() -> String {
        format!("agent:{}", uuid::Uuid::new_v4())
    }

    /// The style counterpart of [`agent_id`].
    fn style_id() -> String {
        format!("custom:{}", uuid::Uuid::new_v4())
    }

    /// A timestamp that is "now" - newer than any seeded row, and inside the
    /// 24 h future cap the record validator enforces.
    fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// Seeds the account store with one already-pushed agent and returns its id.
    fn seed_pushed_agent(hash: &str, id: &str, updated_at: i64) {
        let store = crate::account_scope::AccountAgentsStore {
            custom_agents: vec![crate::account_scope::AccountAgent {
                id: id.to_string(),
                name: "Original".to_string(),
                hint: "old hint".to_string(),
                sync_id: Some(crate::sync::domain::AgentItem::stable_sync_id(id)),
                updated_at: Some(updated_at),
                device_id: Some("dev-local".to_string()),
                deleted_at: None,
                dirty: false,
            }],
        };
        crate::account_scope::save_account_agents(hash, &store).unwrap();
    }

    fn local_agent(hash: &str, id: &str) -> Option<crate::account_scope::AccountAgent> {
        crate::account_scope::load_account_agents(hash)
            .custom_agents
            .into_iter()
            .find(|a| a.id == id)
    }

    /// A local CREATE during the pass must not disappear. Pre-fix the row was
    /// dropped by the stale snapshot and existed in no store at all.
    #[test]
    fn concurrent_agent_create_during_inflight_pass_survives() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-create@example.com");
        let seed = agent_id();
        let created_id = agent_id();
        seed_pushed_agent(&hash, &seed, 1_000);

        let mut drive = FakeDrive::new();
        let hash_c = hash.clone();
        let id_c = created_id.clone();
        drive.on_partitioned_put = Some(Box::new(move || {
            crate::account_scope::upsert_account_agent(
                &hash_c,
                &id_c,
                "Brand New",
                "typed while syncing",
            );
        }));

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        sync_agents_domain(&mut drive, &hash, &mut meta, &mut store)
            .expect("the pass must still succeed");

        let created =
            local_agent(&hash, &created_id).expect("a row created during the pass must survive it");
        assert_eq!(created.name, "Brand New");
        assert!(
            created.dirty,
            "it must stay dirty so the next pass actually uploads it"
        );
        assert!(
            local_agent(&hash, &seed).is_some(),
            "the pre-existing row must also survive"
        );
    }

    /// A local EDIT during the pass must not be reverted by the older payload.
    #[test]
    fn concurrent_agent_edit_during_inflight_pass_survives() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-edit@example.com");
        let id = agent_id();
        seed_pushed_agent(&hash, &id, 1_000);

        let mut drive = FakeDrive::new();
        let hash_c = hash.clone();
        let id_c = id.clone();
        drive.on_partitioned_put = Some(Box::new(move || {
            crate::account_scope::upsert_account_agent(
                &hash_c,
                &id_c,
                "Edited Mid Pass",
                "new hint",
            );
        }));

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        sync_agents_domain(&mut drive, &hash, &mut meta, &mut store)
            .expect("the pass must still succeed");

        let row = local_agent(&hash, &id).expect("row must survive");
        assert_eq!(
            row.name, "Edited Mid Pass",
            "the edit made during the pass must win over the older snapshot"
        );
        assert!(row.dirty, "and it must be queued for the next push");
    }

    /// A local DELETE during the pass must not be undone: the tombstone is
    /// newer than the payload the pass uploaded, so it has to be kept.
    #[test]
    fn concurrent_agent_delete_during_inflight_pass_survives() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-delete@example.com");
        let id = agent_id();
        seed_pushed_agent(&hash, &id, 1_000);

        let mut drive = FakeDrive::new();
        let hash_c = hash.clone();
        let id_c = id.clone();
        drive.on_partitioned_put = Some(Box::new(move || {
            assert!(
                crate::account_scope::delete_account_agent(&hash_c, &id_c),
                "the delete must be durably written during the pass"
            );
        }));

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        sync_agents_domain(&mut drive, &hash, &mut meta, &mut store)
            .expect("the pass must still succeed");

        let row = local_agent(&hash, &id).expect("the tombstone row must remain");
        assert!(
            row.deleted_at.is_some(),
            "the delete must survive, not be resurrected by the older payload"
        );
        assert!(row.dirty, "and the tombstone must still be uploaded");
    }

    /// A REMOTE record that is newer than everything local must still win: the
    /// reconcile is not a blanket "keep local". This is the guard that keeps the
    /// fix from reintroducing the rescue-style permanent-dirty loop.
    #[test]
    fn newer_remote_record_still_wins_over_the_local_row() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-remote@example.com");
        let id = agent_id();
        seed_pushed_agent(&hash, &id, 1_000);

        // Remote carries the same record with a newer revision.
        let remote = format!(
            "{{\"v\":1,\"entries\":[{{\"syncId\":\"{}\",\"businessKey\":\"{}\",\"name\":\"Remote Wins\",\"hint\":\"remote hint\",\"updatedAt\":{},\"deletedAt\":null,\"deviceId\":\"dev-peer\"}}]}}\n",
            crate::sync::domain::AgentItem::stable_sync_id(&id),
            id,
            now_ms(),
        );
        let mut drive = FakeDrive::new();
        let folder = format!(
            "folder:{}",
            crate::sync::account_partition::folder_name(&hash).unwrap()
        );
        drive.partition_files.insert(
            folder,
            vec![DomainFileMeta {
                file_id: "peer-file".to_string(),
                name: crate::sync::drive::AGENTS_FILE.to_string(),
                version: Some("3".to_string()),
                size: None,
            }],
        );
        drive
            .contents
            .insert("peer-file".to_string(), remote.into_bytes());

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        sync_agents_domain(&mut drive, &hash, &mut meta, &mut store)
            .expect("the pass must succeed");

        let row = local_agent(&hash, &id).expect("row must exist");
        assert_eq!(
            row.name, "Remote Wins",
            "a newer remote record must still replace the local row"
        );
        assert!(
            !row.dirty,
            "a row that lost LWW must not stay dirty, or every pass re-uploads it"
        );
    }

    /// The same rule applies to styles, which share the contract.
    #[test]
    fn concurrent_style_create_during_inflight_pass_survives() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-style@example.com");
        let seed = style_id();
        let created_id = style_id();
        crate::account_scope::save_account_styles(
            &hash,
            &crate::account_scope::AccountStylesStore {
                custom_styles: vec![crate::account_scope::AccountStyle {
                    id: seed.clone(),
                    name: "Seed".to_string(),
                    hint: "h".to_string(),
                    sync_id: Some(crate::sync::domain::StyleItem::stable_sync_id(&seed)),
                    updated_at: Some(1_000),
                    device_id: Some("dev-local".to_string()),
                    deleted_at: None,
                    dirty: false,
                }],
            },
        )
        .unwrap();

        let mut drive = FakeDrive::new();
        let hash_c = hash.clone();
        let id_c = created_id.clone();
        drive.on_partitioned_put = Some(Box::new(move || {
            crate::account_scope::upsert_account_style(
                &hash_c,
                &id_c,
                "Brand New Style",
                "typed while syncing",
            );
        }));

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::StyleDirtyStore;
        sync_styles_domain(&mut drive, &hash, &mut meta, &mut store)
            .expect("the pass must still succeed");

        let styles = crate::account_scope::load_account_styles(&hash).custom_styles;
        let created = styles
            .iter()
            .find(|s| s.id == created_id)
            .expect("a style created during the pass must survive it");
        assert_eq!(created.name, "Brand New Style");
        assert!(created.dirty, "and stay dirty for the next push");
        assert!(
            styles.iter().any(|s| s.id == seed),
            "the pre-existing style must survive too"
        );
    }

    /// REPEATED SYNC: an edit preserved by the reconcile must converge on the
    /// next pass - re-stamped above the account high-water mark, uploaded, and
    /// marked clean. This is what proves the fix cannot reintroduce the
    /// rescue-style "permanently dirty, re-uploaded forever" loop.
    #[test]
    fn concurrent_edit_converges_and_goes_clean_on_the_next_pass() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-converge@example.com");
        let id = agent_id();
        seed_pushed_agent(&hash, &id, 1_000);

        // Pass 1: the edit lands while the upload is in flight.
        let mut drive1 = FakeDrive::new();
        let hash_c = hash.clone();
        let id_c = id.clone();
        drive1.on_partitioned_put = Some(Box::new(move || {
            crate::account_scope::upsert_account_agent(&hash_c, &id_c, "Converged", "hint");
        }));
        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        sync_agents_domain(&mut drive1, &hash, &mut meta, &mut store).expect("pass 1");

        let preserved = local_agent(&hash, &id).expect("row survives pass 1");
        assert_eq!(preserved.name, "Converged");
        assert!(preserved.dirty, "still queued for upload");

        // Pass 2: no further local edits. The dirty row must now be stamped
        // above the high-water mark, pushed, and written back clean.
        let mut drive2 = FakeDrive::new();
        sync_agents_domain(&mut drive2, &hash, &mut meta, &mut store).expect("pass 2");

        let settled = local_agent(&hash, &id).expect("row survives pass 2");
        assert_eq!(settled.name, "Converged");
        assert!(
            !settled.dirty,
            "after the push the row must be clean, or every later pass re-uploads it"
        );
        assert!(
            settled.updated_at.unwrap_or(0) > 1_000,
            "the re-stamp must move the row above its pre-edit revision"
        );

        // Pass 3: the remote now carries the converged payload, so a settled
        // row must not keep forcing uploads.
        let mut drive3 = FakeDrive::new();
        drive3.partition_files = drive2.partition_files.clone();
        drive3.contents = drive2.contents.clone();
        sync_agents_domain(&mut drive3, &hash, &mut meta, &mut store).expect("pass 3");
        assert_eq!(
            drive3.partition_put_count, 0,
            "a converged row must not keep forcing uploads"
        );
        assert!(
            !local_agent(&hash, &id).expect("row").dirty,
            "and must stay clean"
        );
    }

    /// COMPETING MUTATIONS: several local writes arriving in the same window
    /// must all survive. The reconcile keys on identity + `dirty`, so the order
    /// in which they land inside the window must not matter.
    #[test]
    fn competing_local_mutations_in_one_window_all_survive() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-multi@example.com");
        let seed = agent_id();
        let a = agent_id();
        let b = agent_id();
        let c = agent_id();
        seed_pushed_agent(&hash, &seed, 1_000);

        let mut drive = FakeDrive::new();
        let (h1, h2, h3) = (hash.clone(), hash.clone(), hash.clone());
        let (a2, b2, c2) = (a.clone(), b.clone(), c.clone());
        drive.on_partitioned_put = Some(Box::new(move || {
            // Three mutations, all inside the same pre-network window.
            crate::account_scope::upsert_account_agent(&h1, &a2, "A", "ha");
            crate::account_scope::upsert_account_agent(&h2, &b2, "B", "hb");
            crate::account_scope::upsert_account_agent(&h3, &c2, "C", "hc");
        }));

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        sync_agents_domain(&mut drive, &hash, &mut meta, &mut store).expect("pass");

        for (id, name) in [(&a, "A"), (&b, "B"), (&c, "C")] {
            let row = local_agent(&hash, id)
                .unwrap_or_else(|| panic!("{name} must survive the competing window"));
            assert_eq!(row.name, name);
            assert!(row.dirty, "{name} must still be queued for upload");
        }
        assert!(
            local_agent(&hash, &seed).is_some(),
            "the pre-existing row must survive too"
        );
    }

    /// FAILURE DURING THE PASS: when the upload fails the pass aborts before
    /// any merge write-back, so the local store must be left exactly as it was.
    /// This is the case the reconcile must never turn into a partial write.
    #[test]
    fn a_failed_upload_leaves_the_local_store_untouched() {
        let _guard = crate::sync::stores::store_test_guard();
        let hash = crate::sync::metadata::account_hash_from_email("race-fail@example.com");
        let id = agent_id();
        seed_pushed_agent(&hash, &id, 1_000);

        let mut drive = FakeDrive::new();
        drive.partition_put_error = Some(SyncError::Retryable("network down".to_string()));

        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::AgentDirtyStore;
        let err = sync_agents_domain(&mut drive, &hash, &mut meta, &mut store)
            .expect_err("the pass must surface the upload failure");
        assert!(matches!(err, SyncError::Retryable(_)));

        let row = local_agent(&hash, &id).expect("the local row must still exist");
        assert_eq!(row.name, "Original", "and must be untouched");
        assert!(!row.dirty, "a failed push must not fabricate sync state");
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

    // ── #21 write-side deterministic ordering ─────────────────────────────
    //
    // `to_bytes` deliberately does NOT sort: the deterministic order is the
    // caller's obligation, and these tests pin the four production sorters to
    // a byte-exact canonical encoding. A shuffled input must produce the same
    // bytes as a canonically ordered one, otherwise two devices holding the
    // same winner set emit different files and CAS ping-pongs forever.
    //
    // The expected strings below are written out by hand, not produced by the
    // code under test — a test that rebuilt the expectation from the sorter
    // would pass no matter what the sorter did.

    fn dict_wire(
        sync_id: &str,
        spoken: &str,
        corrected: &str,
        ts: i64,
        device: &str,
    ) -> DictionaryItem {
        DictionaryItem {
            sync_id: sync_id.to_string(),
            spoken: spoken.to_string(),
            corrected: corrected.to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: ts,
            device_id: device.to_string(),
        }
    }

    fn snippet_wire(
        sync_id: &str,
        trigger: &str,
        expansion: &str,
        ts: i64,
        device: &str,
    ) -> SnippetItem {
        SnippetItem {
            sync_id: sync_id.to_string(),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: ts,
            device_id: device.to_string(),
        }
    }

    fn setting_wire(key: &str, value: &str, ts: i64, device: &str) -> SettingsItem {
        SettingsItem {
            key: key.to_string(),
            value: value.to_string(),
            updated_at: ts,
            device_id: device.to_string(),
            deleted_at: None,
        }
    }

    /// Deliberately reverse-canonical input: `beta` before both `alpha`s, and
    /// the two `alpha`s in DESCENDING syncId order so the secondary key is
    /// load-bearing (businessKey alone cannot order them).
    #[test]
    fn write_side_dictionary_sort_is_deterministic_for_shuffled_input() {
        let shuffled = vec![
            dict_wire(
                "22222222-2222-4222-8222-222222222222",
                " Beta ",
                "B",
                3,
                "dev-b",
            ),
            dict_wire(
                "33333333-3333-4333-8333-333333333333",
                "  Alpha",
                "C",
                2,
                "dev-c",
            ),
            dict_wire(
                "11111111-1111-4111-8111-111111111111",
                "alpha",
                "A",
                1,
                "dev-a",
            ),
        ];
        let mut items = shuffled;
        sort_dictionary_items(&mut items);
        let bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: items,
        }
        .to_bytes();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            concat!(
                r#"{"v":1,"entries":["#,
                r#"{"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"alpha","spoken":"alpha","corrected":"A","isEnabled":true,"updatedAt":1,"deletedAt":null,"deviceId":"dev-a"},"#,
                r#"{"syncId":"33333333-3333-4333-8333-333333333333","businessKey":"alpha","spoken":"  Alpha","corrected":"C","isEnabled":true,"updatedAt":2,"deletedAt":null,"deviceId":"dev-c"},"#,
                r#"{"syncId":"22222222-2222-4222-8222-222222222222","businessKey":"beta","spoken":" Beta ","corrected":"B","isEnabled":true,"updatedAt":3,"deletedAt":null,"deviceId":"dev-b"}"#,
                "]}",
                "\n",
            ),
            "dict writes must be ordered by (businessKey, syncId), whatever the input order"
        );
    }

    /// Same contract for snippets; the input is shuffled AND the two records
    /// that tie on businessKey are given descending syncIds.
    #[test]
    fn write_side_snippet_sort_is_deterministic_for_shuffled_input() {
        let shuffled = vec![
            snippet_wire(
                "33333333-3333-4333-8333-333333333333",
                " Beta ",
                "C",
                3,
                "dev-c",
            ),
            snippet_wire(
                "11111111-1111-4111-8111-111111111111",
                "alpha",
                "A",
                1,
                "dev-a",
            ),
            snippet_wire(
                "22222222-2222-4222-8222-222222222222",
                "  Alpha",
                "B",
                2,
                "dev-b",
            ),
        ];
        let mut items = shuffled;
        sort_snippet_items(&mut items);
        let bytes = SnippetEnvelope {
            v: ENVELOPE_V1,
            entries: items,
        }
        .to_bytes();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            concat!(
                r#"{"v":1,"entries":["#,
                r#"{"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"alpha","trigger":"alpha","expansion":"A","isEnabled":true,"updatedAt":1,"deletedAt":null,"deviceId":"dev-a"},"#,
                r#"{"syncId":"22222222-2222-4222-8222-222222222222","businessKey":"alpha","trigger":"  Alpha","expansion":"B","isEnabled":true,"updatedAt":2,"deletedAt":null,"deviceId":"dev-b"},"#,
                r#"{"syncId":"33333333-3333-4333-8333-333333333333","businessKey":"beta","trigger":" Beta ","expansion":"C","isEnabled":true,"updatedAt":3,"deletedAt":null,"deviceId":"dev-c"}"#,
                "]}",
                "\n",
            ),
            "snippet writes must be ordered by (businessKey, syncId)"
        );
    }

    /// #13 wire half: `deletedAt` is OMITTED when absent, which is exactly what
    /// keeps the four canonical fixtures byte-identical now that the field
    /// exists, and only appears when a tombstone is really carried.
    #[test]
    fn write_side_settings_sort_is_deterministic_and_omits_absent_tombstone() {
        let shuffled = vec![
            setting_wire("snippets_enabled", "false", 3, "dev-c"),
            setting_wire("ai_polish_style", "default", 2, "dev-b"),
            setting_wire("language", "en", 1, "dev-a"),
        ];
        let mut items = shuffled;
        sort_settings_items(&mut items);
        let bytes = SettingsEnvelope {
            v: ENVELOPE_V1,
            entries: items,
        }
        .to_bytes();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            concat!(
                r#"{"v":1,"entries":["#,
                r#"{"key":"ai_polish_style","value":"default","updatedAt":2,"deviceId":"dev-b"},"#,
                r#"{"key":"language","value":"en","updatedAt":1,"deviceId":"dev-a"},"#,
                r#"{"key":"snippets_enabled","value":"false","updatedAt":3,"deviceId":"dev-c"}"#,
                "]}",
                "\n",
            ),
            "settings writes must be key-sorted and must NOT gain a \"deletedAt\":null"
        );

        // And a real tombstone IS emitted, so the retraction reaches Drive.
        let mut with_tomb = vec![setting_wire("language", "en", 1, "dev-a")];
        with_tomb[0].deleted_at = Some(9);
        sort_settings_items(&mut with_tomb);
        assert_eq!(
            String::from_utf8(
                SettingsEnvelope {
                    v: ENVELOPE_V1,
                    entries: with_tomb
                }
                .to_bytes()
            )
            .unwrap(),
            concat!(
                r#"{"v":1,"entries":["#,
                r#"{"key":"language","value":"en","updatedAt":1,"deviceId":"dev-a","deletedAt":9}"#,
                "]}",
                "\n",
            ),
            "a carried tombstone must appear on the wire, after deviceId"
        );
    }

    /// #21 stats: the production stats writer orders by (day, eventId). The
    /// input puts two events on the SAME day in descending eventId order, so
    /// `day` alone is not enough and the secondary key is pinned too.
    #[test]
    fn write_side_stats_sort_is_deterministic_for_shuffled_input() {
        let mk = |event_id: &str, day: &str, ts: i64| StatsItem {
            event_id: event_id.to_string(),
            day: day.to_string(),
            timestamp_ms: ts,
            words: Some(2),
            chars: Some(4),
            duration_ms: Some(6),
            updated_at: None,
            device_id: None,
        };
        let shuffled = vec![
            mk("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "2026-02-01", 20),
            mk("cccccccc-cccc-4ccc-8ccc-cccccccccccc", "2026-01-02", 30),
            mk("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "2026-01-02", 10),
        ];
        let mut items = shuffled;
        shards::sort_stats_records(&mut items);
        let bytes = StatsEnvelope {
            v: ENVELOPE_V1,
            entries: items,
        }
        .to_bytes();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            concat!(
                r#"{"v":1,"entries":["#,
                r#"{"eventId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","day":"2026-01-02","timestampMs":10,"words":2,"chars":4,"durationMs":6},"#,
                r#"{"eventId":"cccccccc-cccc-4ccc-8ccc-cccccccccccc","day":"2026-01-02","timestampMs":30,"words":2,"chars":4,"durationMs":6},"#,
                r#"{"eventId":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","day":"2026-02-01","timestampMs":20,"words":2,"chars":4,"durationMs":6}"#,
                "]}",
                "\n",
            ),
            "stats writes must be ordered by (day, eventId)"
        );
    }

    // ── #7 fencing on the consolidation path ───────────────────────────────
    //
    // The duplicate-consolidation loop deletes only ids that were decoded
    // successfully. These tests drive the path where a push actually HAPPENS
    // (so consolidation runs) alongside a sibling that must survive it —
    // the case the existing "no put happened" tests cannot reach.

    #[test]
    fn a_push_never_deletes_a_corrupt_sibling_while_consolidating() {
        let valid = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("remoteonly", 100)],
        }
        .to_bytes();
        let corrupt = b"{ not json".to_vec();
        // id-1 sorts BEFORE id-2, so id-1 becomes the write target and
        // consolidation walks id-2 looking for duplicates to shed.
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-1", "1", valid.clone())
            .with_file("dictionary.json", "id-2", "2", corrupt.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("localonly", 100)]);
        store.has_dirty = false;

        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(drive.put_count, 1, "the local-only row forces a push");
        assert_eq!(
            drive.delete_count, 0,
            "consolidation must only ever delete VALID duplicates"
        );
        assert_eq!(
            drive.contents.get("id-2"),
            Some(&corrupt),
            "the corrupt sibling may hold data we cannot read; it stays byte-for-byte"
        );
        let written = String::from_utf8(drive.last_put.clone().unwrap()).unwrap();
        assert!(written.contains("remoteonly"), "{written}");
        assert!(written.contains("localonly"), "{written}");
    }

    #[test]
    fn a_push_never_deletes_a_future_sibling_while_consolidating() {
        let valid = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("remoteonly", 100)],
        }
        .to_bytes();
        let future = br#"{"v":2,"entries":[{"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"newer","spoken":"newer","corrected":"newer","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"newer-device"}]}"#.to_vec();
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-1", "1", valid.clone())
            .with_file("dictionary.json", "id-2", "2", future.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("localonly", 100)]);
        store.has_dirty = false;

        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(drive.put_count, 1, "the local-only row forces a push");
        assert_eq!(
            drive.delete_count, 0,
            "a future envelope is owned by a newer client, never shed as a duplicate"
        );
        assert_eq!(
            drive.contents.get("id-2"),
            Some(&future),
            "the newer client's record must be intact"
        );
    }

    /// A file whose records are ALL structurally malformed decodes to an empty
    /// (but readable) envelope. It must NOT be mistaken for a valid sibling
    /// whose contents are part of the merged payload: with non-empty local
    /// state the pass repairs the target, and if that target happened to be
    /// the all-malformed file the repair is a genuine overwrite. What must
    /// never happen is that a VALID sibling is shed while the malformed one
    /// survives as the new target — that would lose the valid file's records.
    #[test]
    fn an_all_malformed_sibling_is_repaired_and_the_valid_sibling_survives() {
        let valid = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![dict_item("remoteonly", 100)],
        }
        .to_bytes();
        let all_bad = br#"{"v":1,"entries":[{"syncId":"22222222-2222-4222-8222-222222222222","spoken":"broken","corrected":"missing isEnabled","updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-b"}]}"#.to_vec();
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-1", "1", valid.clone())
            .with_file("dictionary.json", "id-2", "2", all_bad.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("localonly", 100)]);
        store.has_dirty = false;

        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "{res:?}");
        let written = String::from_utf8(drive.last_put.clone().unwrap()).unwrap();
        assert!(written.contains("remoteonly"), "{written}");
        assert!(written.contains("localonly"), "{written}");
        assert_eq!(
            drive.delete_count, 0,
            "the valid sibling was the write target; nothing needed shedding"
        );
        assert_eq!(
            store
                .items
                .iter()
                .map(|i| i.spoken.as_str())
                .collect::<Vec<_>>(),
            vec!["localonly", "remoteonly"]
        );
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

    /// F5: a listing that already reports over-cap must skip the content
    /// download, not re-fetch an oversized file every pass only to discard it.
    /// (`with_file` models a real listing, which always carries the size.)
    #[test]
    fn oversized_remote_content_is_not_downloaded() {
        let oversized = vec![b'x'; MAX_DOMAIN_BYTES + 1];
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", oversized);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(
            matches!(res, Err(SyncError::Rejected(_))),
            "oversized must still surface Rejected"
        );
        assert_eq!(
            drive.get_count, 0,
            "known-oversized content must never be downloaded"
        );
    }

    /// F5 fallback: a listing WITHOUT a size still downloads and checks, so
    /// trimmed listings (or folders) change nothing.
    #[test]
    fn unknown_listing_size_still_downloads_and_checks() {
        let oversized = vec![b'x'; MAX_DOMAIN_BYTES + 1];
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", oversized);
        // Simulate a listing that omits `size` (folders, trimmed fields).
        drive.files[0].size = None;
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(
            matches!(res, Err(SyncError::Rejected(_))),
            "oversized must still surface Rejected via the download path"
        );
        assert_eq!(
            drive.get_count, 1,
            "unknown size must fall back to download-and-check"
        );
    }

    /// F5: hostile siblings at once (oversized + corrupt + future, no valid
    /// file) surface Rejected — never a Fatal latch — with every remote left
    /// intact. Malformed or oversized remote content must not wedge scheduling.
    #[test]
    fn hostile_siblings_without_valid_never_fatal() {
        let oversized = vec![b'x'; MAX_DOMAIN_BYTES + 1];
        let corrupt = b"{ not json".to_vec();
        let future = br#"{"v":2,"entries":[]}"#.to_vec();
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-big", "1", oversized.clone())
            .with_file("dictionary.json", "id-corrupt", "2", corrupt.clone())
            .with_file("dictionary.json", "id-future", "3", future.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(
            matches!(res, Err(SyncError::Rejected(_))),
            "hostile-only remote must be Rejected, never Fatal: {res:?}"
        );
        assert_eq!(
            drive.put_count, 0,
            "nothing may be written over hostile remotes"
        );
        assert_eq!(
            drive.contents.get("id-big"),
            Some(&oversized),
            "oversized remote intact"
        );
        assert_eq!(
            drive.contents.get("id-corrupt"),
            Some(&corrupt),
            "corrupt remote intact"
        );
        assert_eq!(
            drive.contents.get("id-future"),
            Some(&future),
            "future remote intact"
        );
    }

    /// F6 has_dirty re-read: the push decision must observe post-stamp
    /// dirtiness. The local row is dirty but already lost LWW to a newer
    /// remote revision, so the merge equals the valid remote exactly —
    /// `state_differs` is false and the `!valid_found` term cannot fire.
    /// `has_dirty` is the ONLY term that can trigger this push; without it
    /// the row stays dirty forever (uploaded never, cleared never).
    #[test]
    fn dirty_loser_still_pushes_via_has_dirty_then_goes_clean() {
        let _guard = crate::sync::stores::store_test_guard();
        let account_hash = format!(
            "test-dirtypush-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let sync_id = "11111111-1111-4111-8111-111111111111";
        // Local: dirty, but older than the remote revision below.
        let mut all = crate::dictionary::load_dictionary_internal().unwrap_or_default();
        all.push(crate::dictionary::DictionaryEntry {
            id: sync_id.to_string(),
            spoken: "hello".to_string(),
            corrected: "oldlocal".to_string(),
            kind: "correction".to_string(),
            created_at: Some(1000),
            deleted_at: None,
            updated_at: Some(1000),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: true,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        crate::dictionary::save_dictionary_internal(&all).unwrap();
        // Remote: valid file, same key, strictly newer — merge == remote.
        let remote_bytes = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![DictionaryItem {
                sync_id: sync_id.to_string(),
                spoken: "hello".to_string(),
                corrected: "newremote".to_string(),
                kind: "correction".to_string(),
                is_enabled: true,
                deleted_at: None,
                updated_at: 2000,
                device_id: "device-peer".to_string(),
            }],
        }
        .to_bytes();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-a", "1", remote_bytes);
        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::DictionaryDirtyStore;
        sync_dictionary_domain(&mut drive, &account_hash, &mut meta, &mut store).expect("pass");
        assert_eq!(
            drive.put_count, 1,
            "has_dirty alone must trigger the push when the merge already equals remote"
        );
        let after = crate::dictionary::load_dictionary_internal().unwrap_or_default();
        let row = after
            .iter()
            .find(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .expect("row survives");
        assert!(
            !row.dirty,
            "the push must clear the flag or it re-uploads forever"
        );
        assert_eq!(
            row.corrected, "newremote",
            "the newer remote winner applies"
        );
        let mut cleanup = crate::dictionary::load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = crate::dictionary::save_dictionary_internal(&cleanup);
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

    /// #7 headline regression: a file whose ENVELOPE is fine but which contains
    /// one structurally-malformed record must be read as "valid, minus the bad
    /// record" — not as corrupt. Reading it as corrupt made `sync_domain` see
    /// `!valid_found` with non-empty local state and PUT a replacement over the
    /// Drive file, destroying every record the file actually held.
    ///
    /// The pushed payload is the proof: it must carry the remote's good record
    /// AND the local-only row, and must NOT carry the malformed ones.
    #[test]
    fn a_malformed_record_no_longer_costs_the_whole_remote_file() {
        let remote = br#"{"v":1,"entries":[
            {"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"remoteonly","spoken":"remoteonly","corrected":"kept","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-a"},
            {"syncId":"22222222-2222-4222-8222-222222222222","businessKey":"broken","spoken":"broken","corrected":"missing isEnabled","updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-b"},
            {"syncId":"33333333-3333-4333-8333-333333333333","businessKey":"wrongtype","spoken":"wrongtype","corrected":"updatedAt is a string","isEnabled":true,"updatedAt":"1713471000123","deletedAt":null,"deviceId":"device-b"}
        ]}"#.to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", remote.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("localonly", 100)]);
        store.has_dirty = false;

        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "a partly-bad file is not an error: {res:?}");
        assert_eq!(drive.put_count, 1, "the local-only row still needs pushing");
        let written = String::from_utf8(drive.last_put.clone().expect("a payload was pushed"))
            .expect("utf8 payload");
        assert!(
            written.contains("remoteonly"),
            "the remote's good record must survive the repair — payload: {written}"
        );
        assert!(
            written.contains("localonly"),
            "the local-only row must be pushed: {written}"
        );
        assert!(
            !written.contains("\"broken\"") && !written.contains("wrongtype"),
            "malformed records are dropped, never resurrected: {written}"
        );
        // And the record really came back into local state.
        assert_eq!(
            store
                .items
                .iter()
                .map(|i| i.spoken.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            ["localonly", "remoteonly"].into_iter().collect(),
        );
    }

    /// The boundary the audit calls out explicitly: a file that is malformed at
    /// the ENVELOPE level (unparseable JSON) still has no recoverable content,
    /// so the existing repair-by-push behaviour is correct and unchanged —
    /// `corrupt_within_size_is_repaired_via_push` pins it. What must NEVER
    /// happen is that envelope level being inferred from a record level fault.
    #[test]
    fn envelope_corruption_is_still_repaired_but_record_faults_are_not() {
        let corrupt = b"{ not json".to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", corrupt);
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("localonly", 100)]);
        store.has_dirty = false;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "unparseable JSON is repaired, not rejected");
        assert_eq!(drive.put_count, 1);
        let written = String::from_utf8(drive.last_put.clone().unwrap()).unwrap();
        assert!(written.contains("localonly"));
    }

    /// Valid + corrupt SIBLING files coexisting: the valid file's records are
    /// merged in, the corrupt sibling is neither trusted nor deleted, and no
    /// write is issued when the merged set already equals the valid remote.
    #[test]
    fn a_corrupt_sibling_never_discards_the_valid_files_records() {
        let valid = DictionaryEnvelope {
            v: ENVELOPE_V1,
            entries: vec![
                DictionaryItem {
                    sync_id: "11111111-1111-4111-8111-111111111111".to_string(),
                    spoken: "alpha".to_string(),
                    corrected: "A".to_string(),
                    kind: "correction".to_string(),
                    is_enabled: true,
                    deleted_at: None,
                    updated_at: 100,
                    device_id: "device-a".to_string(),
                },
                DictionaryItem {
                    sync_id: "22222222-2222-4222-8222-222222222222".to_string(),
                    spoken: "beta".to_string(),
                    corrected: "B".to_string(),
                    kind: "correction".to_string(),
                    is_enabled: true,
                    deleted_at: None,
                    updated_at: 100,
                    device_id: "device-a".to_string(),
                },
            ],
        }
        .to_bytes();
        let corrupt = b"{ not json".to_vec();
        let mut drive = FakeDrive::new()
            .with_file("dictionary.json", "id-a", "1", valid.clone())
            .with_file("dictionary.json", "id-b", "2", corrupt.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(Vec::new());
        store.has_dirty = false;

        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(
            drive.put_count, 0,
            "merged == valid remote, so nothing is written"
        );
        assert_eq!(
            drive.delete_count, 0,
            "a corrupt sibling must never be deleted — it may hold data we cannot read"
        );
        assert_eq!(
            drive.contents.get("id-b"),
            Some(&corrupt),
            "the corrupt sibling is left byte-for-byte intact"
        );
        assert_eq!(
            store
                .items
                .iter()
                .map(|i| i.spoken.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"],
            "both valid sibling records survive"
        );
    }

    /// A file whose every record is malformed still has a readable envelope:
    /// with empty local state there is nothing to heal, so it must be left
    /// exactly as-is rather than replaced with an empty envelope.
    #[test]
    fn an_all_malformed_file_is_left_untouched_when_local_is_empty() {
        let all_bad = br#"{"v":1,"entries":[
            {"syncId":"22222222-2222-4222-8222-222222222222","businessKey":"broken","spoken":"broken","corrected":"missing isEnabled","updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-b"}
        ]}"#.to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", all_bad.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::<DictionaryItem>::new(Vec::new());
        store.has_dirty = false;
        let res = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(drive.put_count, 0, "nothing to heal means no write");
        assert_eq!(
            drive.contents.get("id-1"),
            Some(&all_bad),
            "the remote must not be rewritten"
        );
    }

    /// The saturation guard in `declared_envelope_version` is a fencing guard:
    /// `v = 2^32+1` truncated to 1 under `as i32`, which read as "v1 corrupt" —
    /// a state `sync_domain` is allowed to overwrite. It must be fenced like
    /// any other future envelope.
    #[test]
    fn a_far_future_envelope_version_is_never_overwritten() {
        let future = br#"{"v":4294967297,"entries":[{"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"newer","spoken":"newer","corrected":"newer","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"newer-device"}]}"#.to_vec();
        let mut drive = FakeDrive::new().with_file("dictionary.json", "id-1", "1", future.clone());
        let mut meta = SyncMetadata::default();
        let mut store = MemStore::new(vec![dict_item("hello", 100)]);
        store.has_dirty = true;
        let _ = sync_dictionary_domain(&mut drive, "hash", &mut meta, &mut store);
        assert_eq!(
            drive.put_count, 0,
            "a 2^32+1 envelope is future, not corrupt"
        );
        assert_eq!(
            drive.contents.get("id-1"),
            Some(&future),
            "the newer client's data must be intact"
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
    fn expansion_entry_syncs_without_failing_the_roundtrip_gate() {
        // Regression (audit finding C1): `kind` is local-only and never
        // serialized (F1), so the reader always reconstitutes it as
        // `correction`. If the payload carried the local `expansion` value,
        // `decode(encode(merged)) != merged` and this engine returned
        // `SyncError::Fatal` - which latched the scheduler and stopped ALL
        // domains from syncing. Reproduced here end-to-end through the real
        // engine against a real (temp-rooted) dictionary store.
        let _guard = crate::sync::stores::store_test_guard();
        let account = format!("c1-expansion-{}", uuid::Uuid::new_v4());
        let entry = crate::dictionary::DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "expandme".to_string(),
            corrected: "expanded phrase".to_string(),
            kind: crate::dictionary::KIND_EXPANSION.to_string(),
            created_at: Some(1_000),
            deleted_at: None,
            updated_at: Some(2_000),
            device_id: Some("00000000-0000-4000-8000-0000000000aa".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut rows = crate::dictionary::load_dictionary_internal().unwrap_or_default();
        rows.push(entry);
        crate::dictionary::save_dictionary_internal(&rows).unwrap();

        let mut drive = FakeDrive::new();
        let mut meta = SyncMetadata::default();
        let mut store = crate::sync::stores::DictionaryDirtyStore;
        let res = sync_dictionary_domain(&mut drive, &account, &mut meta, &mut store);

        match res {
            Err(SyncError::Fatal(msg)) => panic!(
                "an expansion entry must not fail the round-trip gate (it used \
                 to return Fatal and latch the scheduler): {msg}"
            ),
            other => assert!(other.is_ok(), "unexpected sync failure: {other:?}"),
        }
        assert_eq!(
            drive.put_count, 1,
            "the expansion entry is uploaded like any other entry"
        );

        // And the local annotation survives the merge write-back.
        let after = crate::dictionary::load_dictionary_internal().unwrap_or_default();
        let row = after
            .iter()
            .find(|e| e.sync_account.as_deref() == Some(account.as_str()))
            .expect("row persisted");
        assert_eq!(row.kind, crate::dictionary::KIND_EXPANSION);

        let mut cleanup = crate::dictionary::load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account.as_str()));
        let _ = crate::dictionary::save_dictionary_internal(&cleanup);
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
                let size = Some(bytes.len() as u64);
                self.files.push(DomainFileMeta {
                    file_id: file_id.to_string(),
                    name: name.to_string(),
                    version: Some(version.to_string()),
                    size,
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
                    size: Some(_content.len() as u64),
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
                    size: Some(_content.len() as u64),
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

// ── Stats time shards: engine integration ─────────────────────────────────
// These drive `sync_stats_domain` (sharded) against a content-aware fake
// with production-identical CAS, proving: legacy freeze, month routing,
// union reads, old/new coexistence, per-file failure containment, stale
// retry, liveness gating, convergence, reinstall, rollover, and the
// lifetime semantic invariants (sessions = record count, sums, per-day
// buckets, timestamp multisets).
#[cfg(test)]
mod sharded_stats_tests {
    use super::*;
    use crate::sync::drive::{DomainFileMeta, MAX_DOMAIN_BYTES};
    use crate::sync::error::SyncError;
    use crate::sync::metadata::SyncMetadata;
    use crate::sync::stats_shards as shards;

    const HASH: &str = "test-account-hash";

    fn stat(i: u32, day: &str, words: i64) -> StatsItem {
        StatsItem {
            event_id: format!(
                "{:08x}-0000-4000-8000-{:012x}",
                i,
                (i as u64).wrapping_mul(2654435761) & 0xffffffffffff
            ),
            day: day.to_string(),
            timestamp_ms: 1_700_000_000_000 + i as i64 * 1_000,
            words: Some(words),
            chars: Some(words * 5),
            duration_ms: Some(1_000),
            updated_at: None,
            device_id: None,
        }
    }

    /// F2: the stats max_seen floor must use LWW time (`updated_at`), exactly
    /// like Android's `merged.maxOfOrNull { it.updatedAt }`, never event time
    /// (`timestamp_ms`). A re-merged old dictation (large `timestamp_ms`, small
    /// `updated_at`) must not inflate the floor; an unstamped row (`None`)
    /// contributes 0 and the monotone `update_max_seen` guard makes that a
    /// no-op rather than a regression.
    #[test]
    fn stats_floor_uses_updated_at_not_event_time() {
        let mut old_event = stat(1, "2024-01-01", 10);
        old_event.timestamp_ms = 9_000_000_000_000;
        old_event.updated_at = Some(1_000_000_000_000);
        let unstamped = stat(2, "2024-01-02", 5);
        assert_eq!(
            super::stats_floor_ts(&[old_event, unstamped]),
            1_000_000_000_000,
            "floor must follow updated_at (1M), not timestamp_ms (9T)"
        );
        assert_eq!(
            super::stats_floor_ts(&[]),
            0,
            "empty merge advances nothing"
        );
    }

    fn envelope(rows: &[StatsItem]) -> Vec<u8> {
        StatsEnvelope {
            v: ENVELOPE_V1,
            entries: rows.to_vec(),
        }
        .to_bytes()
    }

    fn parsed_names(entries: &[StatsItem]) -> Vec<String> {
        let mut ids: Vec<String> = entries.iter().map(|e| e.event_id.clone()).collect();
        ids.sort();
        ids
    }

    /// Content-aware fake: production-identical CAS (preferred-file-or-name
    /// match, version freshness, create-on-absent), versioned listing for
    /// liveness, real deletes. Optional stale injection and liveness
    /// blackhole for failure-path tests.
    struct ShardFakeDrive {
        files: Vec<DomainFileMeta>,
        contents: std::collections::HashMap<String, Vec<u8>>,
        puts: Vec<(String, Vec<u8>, Option<String>)>,
        deletes: Vec<String>,
        next_version: u64,
        stale_once: bool,
        concurrent_on_stale: Option<Vec<u8>>,
        blackhole: bool,
    }

    impl ShardFakeDrive {
        fn new() -> Self {
            Self {
                files: Vec::new(),
                contents: std::collections::HashMap::new(),
                puts: Vec::new(),
                deletes: Vec::new(),
                next_version: 100,
                stale_once: false,
                concurrent_on_stale: None,
                blackhole: false,
            }
        }
        fn with_legacy(mut self, rows: &[StatsItem], version: &str) -> Self {
            self.add_file(STATS_FILE, "id-legacy", version, envelope(rows));
            self
        }
        fn with_legacy_bytes(mut self, bytes: Vec<u8>, version: &str) -> Self {
            self.add_file(STATS_FILE, "id-legacy", version, bytes);
            self
        }
        fn with_shard(
            mut self,
            name: &str,
            file_id: &str,
            rows: &[StatsItem],
            version: &str,
        ) -> Self {
            self.add_file(name, file_id, version, envelope(rows));
            self
        }
        fn with_shard_bytes(
            mut self,
            name: &str,
            file_id: &str,
            bytes: Vec<u8>,
            version: &str,
        ) -> Self {
            self.add_file(name, file_id, version, bytes);
            self
        }
        fn add_file(&mut self, name: &str, file_id: &str, version: &str, bytes: Vec<u8>) {
            let size = Some(bytes.len() as u64);
            self.files.push(DomainFileMeta {
                file_id: file_id.to_string(),
                name: name.to_string(),
                version: Some(version.to_string()),
                size,
            });
            self.contents.insert(file_id.to_string(), bytes);
        }
        fn contents_of(&self, name: &str) -> Option<Vec<u8>> {
            let meta = self.files.iter().find(|f| f.name == name)?;
            self.contents.get(&meta.file_id).cloned()
        }
        fn parsed_of(&self, name: &str) -> Vec<StatsItem> {
            self.contents_of(name)
                .and_then(|b| StatsEnvelope::from_bytes(&b))
                .map(|e| e.entries)
                .unwrap_or_default()
        }
        fn put_names(&self) -> Vec<String> {
            self.puts.iter().map(|(n, _, _)| n.clone()).collect()
        }
    }

    impl crate::sync::drive::DomainDriveStore for ShardFakeDrive {
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
            content: &[u8],
            expected_version: Option<&str>,
            preferred_file_id: Option<&str>,
        ) -> Result<String, SyncError> {
            if self.stale_once {
                self.stale_once = false;
                if let Some(concurrent) = self.concurrent_on_stale.take() {
                    // A concurrent writer lands while we retry: bump legacy.
                    if let Some(meta) = self.files.iter_mut().find(|f| f.name == STATS_FILE) {
                        meta.version = Some("v-concurrent".to_string());
                        self.contents.insert(meta.file_id.clone(), concurrent);
                    }
                }
                return Err(SyncError::StaleVersion("v-live".to_string()));
            }
            self.puts.push((
                name.to_string(),
                content.to_vec(),
                expected_version.map(str::to_string),
            ));
            let existing = preferred_file_id
                .and_then(|id| self.files.iter().position(|f| f.file_id == id))
                .or_else(|| self.files.iter().position(|f| f.name == name));
            self.next_version += 1;
            let new_version = format!("rv{}", self.next_version);
            match existing {
                Some(idx) => {
                    let live = self.files[idx].version.clone();
                    let fresh = match (expected_version, live.as_deref()) {
                        (Some(exp), Some(cur)) => exp == cur,
                        (Some(_), None) => false,
                        (None, Some(_)) => false,
                        (None, None) => true,
                    };
                    if !fresh {
                        return Err(SyncError::StaleVersion(
                            live.unwrap_or("<none>".to_string()),
                        ));
                    }
                    if !self.blackhole {
                        self.files[idx].version = Some(new_version.clone());
                        let fid = self.files[idx].file_id.clone();
                        self.contents.insert(fid, content.to_vec());
                    }
                    Ok(new_version)
                }
                None => {
                    let fid = format!("id-created-{}", self.next_version);
                    if !self.blackhole {
                        self.files.push(DomainFileMeta {
                            file_id: fid.clone(),
                            name: name.to_string(),
                            version: Some(new_version.clone()),
                            size: Some(content.len() as u64),
                        });
                        self.contents.insert(fid, content.to_vec());
                    }
                    Ok(new_version)
                }
            }
        }
        fn delete_domain_file(&mut self, file_id: &str) -> Result<(), SyncError> {
            self.deletes.push(file_id.to_string());
            self.files.retain(|f| f.file_id != file_id);
            self.contents.remove(file_id);
            Ok(())
        }
        fn ensure_partition_folder(&mut self, _folder_name: &str) -> Result<String, SyncError> {
            Ok("folder:x".to_string())
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
            Err(SyncError::Fatal("unused".to_string()))
        }
        fn drive_account_email(&mut self) -> Result<Option<String>, SyncError> {
            Ok(Some("test@example.com".to_string()))
        }
    }

    struct TrackRow {
        item: StatsItem,
        dirty: bool,
    }
    /// Dirty-aware store mirroring the production `StatsDirtyStore` rescue:
    /// a dirty local row that ties-or-wins LWW against the merged winner
    /// stays dirty (mid-pass guard), and shard-unroutable rows stay dirty.
    struct TrackStore {
        rows: Vec<TrackRow>,
        saves: usize,
    }
    impl TrackStore {
        fn new(items: Vec<StatsItem>, dirty: bool) -> Self {
            Self {
                rows: items
                    .into_iter()
                    .map(|item| TrackRow { item, dirty })
                    .collect(),
                saves: 0,
            }
        }
        fn items(&self) -> Vec<StatsItem> {
            self.rows.iter().map(|r| r.item.clone()).collect()
        }
        fn any_dirty(&self) -> bool {
            self.rows.iter().any(|r| r.dirty)
        }
        fn is_dirty(&self, event_id: &str) -> bool {
            self.rows
                .iter()
                .any(|r| r.item.event_id == event_id && r.dirty)
        }
        fn push(&mut self, item: StatsItem, dirty: bool) {
            self.rows.push(TrackRow { item, dirty });
        }
    }
    impl DirtyStore for TrackStore {
        type Item = StatsItem;
        fn load(&self, _h: &str) -> Vec<StatsItem> {
            self.items()
        }
        fn stamp_account(&mut self, _h: &str) -> Result<usize, SyncError> {
            Ok(0)
        }
        fn has_dirty(&self, _h: &str) -> bool {
            self.any_dirty()
        }
        fn save_merged(&mut self, _h: &str, m: Vec<StatsItem>) -> Result<(), SyncError> {
            use std::collections::HashMap;
            let dirty_by_id: HashMap<&str, &StatsItem> = self
                .rows
                .iter()
                .filter(|r| r.dirty)
                .map(|r| (r.item.event_id.as_str(), &r.item))
                .collect();
            let merged_ids: std::collections::HashSet<String> =
                m.iter().map(|i| i.event_id.clone()).collect();
            let mut next = Vec::new();
            for item in m {
                if shards::month_of_day(&item.day).is_none() {
                    next.push(TrackRow { item, dirty: true });
                    continue;
                }
                if let Some(local) = dirty_by_id.get(item.event_id.as_str()) {
                    let local_at = local.updated_at.unwrap_or(0);
                    let local_dev = local.device_id.as_deref().unwrap_or("");
                    if crate::sync::clock::cmp_winner(
                        local_at,
                        local_dev,
                        item.updated_at.unwrap_or(0),
                        item.device_id.as_deref().unwrap_or(""),
                    ) != std::cmp::Ordering::Less
                    {
                        next.push(TrackRow {
                            item: (*local).clone(),
                            dirty: true,
                        });
                        continue;
                    }
                }
                next.push(TrackRow { item, dirty: false });
            }
            // Dirty rows absent from the merge are rescued (kept dirty).
            for row in &self.rows {
                if row.dirty && !merged_ids.contains(&row.item.event_id) {
                    next.push(TrackRow {
                        item: row.item.clone(),
                        dirty: true,
                    });
                }
            }
            self.rows = next;
            self.saves += 1;
            Ok(())
        }
    }

    fn sync(
        drive: &mut ShardFakeDrive,
        store: &mut TrackStore,
    ) -> Result<DomainSyncOutcome, SyncError> {
        let mut meta = SyncMetadata::default();
        sync_stats_domain(drive, HASH, &mut meta, store)
    }

    #[test]
    fn read_unions_legacy_and_shards_with_no_put_when_clean() {
        let a = stat(1, "2026-08-01", 10);
        let b = stat(2, "2026-09-01", 20);
        let mut drive = ShardFakeDrive::new()
            .with_legacy(&[a.clone()], "v1")
            .with_shard("stats-2026-09.json", "id-s09", &[b.clone()], "v2");
        let mut store = TrackStore::new(vec![a.clone()], false);
        let out = sync(&mut drive, &mut store).expect("union pull must succeed");
        assert!(!out.pushed, "converged content must not re-upload");
        assert!(out.merged);
        assert!(!out.degraded, "fully readable remote is not degraded");
        assert!(drive.puts.is_empty(), "no PUT when desired equals remote");
        assert_eq!(parsed_names(&store.items()), parsed_names(&[a, b]));
    }

    #[test]
    fn new_dirty_row_routes_to_month_shard_and_legacy_stays_frozen() {
        let a = stat(1, "2026-08-01", 10);
        let n = stat(2, "2026-08-20", 30);
        let mut drive = ShardFakeDrive::new().with_legacy(&[a.clone()], "v1");
        let mut store = TrackStore::new(vec![a.clone(), n.clone()], true);
        let out = sync(&mut drive, &mut store).expect("shard write must succeed");
        assert!(out.pushed);
        assert_eq!(drive.put_names(), vec!["stats-2026-08.json".to_string()]);
        // Shard holds ONLY the non-legacy row: history is never recopied.
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[n.clone()])
        );
        // Legacy untouched: same version, same bytes.
        assert_eq!(
            drive.contents_of(STATS_FILE).unwrap(),
            envelope(&[a.clone()])
        );
        let nid = n.event_id.clone();
        assert_eq!(parsed_names(&store.items()), parsed_names(&[a, n]));
        // Production tie-guard: the uploaded row ties LWW (identical stamp),
        // so it stays dirty locally — exactly like the flat engine.
        assert!(store.is_dirty(&nid));
        // …but the diff-driven engine never re-uploads identical content:
        // the second pass converges with no PUT (no upload loop).
        let out2 = sync(&mut drive, &mut store).expect("second pass must succeed");
        assert!(!out2.pushed);
        assert_eq!(drive.puts.len(), 1, "no repeated upload of identical bytes");
    }

    #[test]
    fn corrupt_legacy_is_never_repaired_while_shards_sync() {
        let n = stat(7, "2026-08-20", 30);
        let before = b"{ not json".to_vec();
        let mut drive = ShardFakeDrive::new().with_legacy_bytes(before.clone(), "v9");
        let mut store = TrackStore::new(vec![n.clone()], true);
        let out = sync(&mut drive, &mut store).expect("shards sync around corrupt legacy");
        assert!(out.pushed);
        assert!(out.degraded, "skipped legacy must surface as degraded");
        assert_eq!(drive.put_names(), vec!["stats-2026-08.json".to_string()]);
        assert_eq!(
            drive.contents_of(STATS_FILE).unwrap(),
            before,
            "frozen legacy is never repaired or rewritten"
        );
    }

    #[test]
    fn old_client_legacy_write_is_visible_with_no_put() {
        let a = stat(1, "2026-08-01", 10);
        let r = stat(9, "2026-08-02", 99); // written by an old client into stats.json
        let mut drive = ShardFakeDrive::new().with_legacy(&[a.clone(), r.clone()], "v5");
        let mut store = TrackStore::new(vec![a.clone()], false);
        let out = sync(&mut drive, &mut store).expect("pull must succeed");
        assert!(!out.pushed, "legacy-owned rows need no shard rewrite");
        assert!(drive.puts.is_empty());
        assert_eq!(parsed_names(&store.items()), parsed_names(&[a, r]));
    }

    #[test]
    fn corrupt_shard_with_content_is_repaired_via_cas() {
        let n = stat(3, "2026-08-05", 30);
        let mut drive = ShardFakeDrive::new().with_shard_bytes(
            "stats-2026-08.json",
            "id-c1",
            b"{ corrupt".to_vec(),
            "vC1",
        );
        let mut store = TrackStore::new(vec![n.clone()], true);
        let out = sync(&mut drive, &mut store).expect("corrupt shard repair must succeed");
        assert!(out.pushed);
        assert_eq!(drive.puts.len(), 1);
        assert_eq!(drive.puts[0].0, "stats-2026-08.json");
        assert_eq!(
            drive.puts[0].2.as_deref(),
            Some("vC1"),
            "repair CAS-targets the corrupt revision"
        );
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[n])
        );
    }

    #[test]
    fn corrupt_shard_with_no_content_is_left_alone() {
        let mut drive = ShardFakeDrive::new().with_shard_bytes(
            "stats-2026-08.json",
            "id-c1",
            b"{ corrupt".to_vec(),
            "vC1",
        );
        let mut store = TrackStore::new(vec![], false);
        let out = sync(&mut drive, &mut store).expect("empty sync must succeed");
        assert!(!out.pushed, "never fabricate a repair with no usable state");
        assert!(drive.puts.is_empty());
        assert_eq!(
            drive.contents_of("stats-2026-08.json").unwrap(),
            b"{ corrupt".to_vec()
        );
    }

    /// #7 fence, stats side. Per-record isolation means a shard whose every
    /// record is structurally malformed decodes to an EMPTY envelope rather
    /// than `None`. Registering that as a valid duplicate would let the
    /// consolidation step DELETE it — losing records this client simply could
    /// not parse. The valid sibling must carry the data and the malformed one
    /// must survive the push.
    #[test]
    fn an_all_malformed_stats_shard_sibling_is_never_shed_by_consolidation() {
        let remote_event = stat(1, "2026-08-01", 10);
        let valid = StatsEnvelope {
            v: ENVELOPE_V1,
            entries: vec![remote_event.clone()],
        }
        .to_bytes();
        let all_bad = br#"{"v":1,"entries":[{"day":"2026-08-05","timestampMs":50}]}"#.to_vec();
        let mut drive = ShardFakeDrive::new()
            .with_shard_bytes("stats-2026-08.json", "id-a", valid.clone(), "vA")
            .with_shard_bytes("stats-2026-08.json", "id-b", all_bad.clone(), "vB");
        let local = stat(2, "2026-08-02", 20);
        let mut store = TrackStore::new(vec![local], true);

        let out = sync(&mut drive, &mut store).expect("sync must succeed");
        assert!(out.pushed, "the local-only event forces an upload");
        assert!(
            drive.deletes.is_empty(),
            "consolidation must never shed a readable-but-empty sibling: {:?}",
            drive.deletes
        );
        assert_eq!(
            drive.contents.get("id-b").cloned(),
            Some(all_bad),
            "the unreadable sibling stays byte-for-byte"
        );
        assert!(
            drive.files.iter().any(|f| f.file_id == "id-b"),
            "and is still listed, not consolidated away"
        );
        assert!(parsed_names(&store.items()).contains(&remote_event.event_id));
    }

    #[test]
    fn oversize_shard_is_skipped_while_healthy_months_sync() {
        let l = stat(1, "2026-08-01", 10);
        let n = stat(2, "2026-09-10", 20);
        let big = vec![0u8; MAX_DOMAIN_BYTES + 1];
        let mut drive = ShardFakeDrive::new()
            .with_legacy(&[l.clone()], "v1")
            .with_shard_bytes("stats-2026-08.json", "id-big", big.clone(), "vB");
        let mut store = TrackStore::new(vec![l.clone(), n.clone()], true);
        let out = sync(&mut drive, &mut store).expect("healthy months sync around oversize");
        assert!(out.pushed);
        assert!(
            out.degraded,
            "skipped oversize shard must surface as degraded"
        );
        assert_eq!(drive.put_names(), vec!["stats-2026-09.json".to_string()]);
        assert_eq!(
            drive.contents_of("stats-2026-08.json").unwrap(),
            big,
            "oversize retained"
        );
        assert_eq!(parsed_names(&store.items()), parsed_names(&[l, n]));
    }

    #[test]
    fn stale_version_retries_with_full_remerge() {
        let local = stat(1, "2026-08-01", 10);
        let remote = stat(2, "2026-08-02", 20);
        let concurrent = stat(3, "2026-08-03", 30);
        let mut drive = ShardFakeDrive::new().with_legacy(&[remote.clone()], "v1");
        drive.stale_once = true;
        // Concurrent old-client write lands in legacy during our retry.
        let mut both = vec![remote.clone(), concurrent.clone()];
        both.sort_by(|a, b| a.day.cmp(&b.day).then_with(|| a.event_id.cmp(&b.event_id)));
        drive.concurrent_on_stale = Some(envelope(&both));
        let mut store = TrackStore::new(vec![local.clone()], true);
        let out = sync(&mut drive, &mut store).expect("stale retry must converge");
        assert!(out.pushed);
        // Final shard content is the re-merged state: local + remote + concurrent,
        // minus legacy-identical rows (remote + concurrent live in legacy).
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[local.clone()])
        );
        assert_eq!(
            parsed_names(&store.items()),
            parsed_names(&[local, remote, concurrent])
        );
    }

    #[test]
    fn liveness_miss_keeps_dirty_and_saves_nothing() {
        let n = stat(4, "2026-08-20", 30);
        let mut drive = ShardFakeDrive::new();
        drive.blackhole = true;
        let mut store = TrackStore::new(vec![n.clone()], true);
        let out = sync(&mut drive, &mut store).expect("liveness miss is non-fatal");
        assert!(!out.pushed);
        assert!(!out.merged);
        assert_eq!(store.saves, 0, "never mark pushed unless live");
        assert!(store.any_dirty(), "rows stay dirty for the next pass");
    }

    #[test]
    fn two_devices_converge_through_shards() {
        let a = stat(1, "2026-08-01", 10);
        let b = stat(2, "2026-08-02", 20);
        let mut drive = ShardFakeDrive::new();
        let mut meta = SyncMetadata::default();
        // Device A writes.
        let mut da = TrackStore::new(vec![a.clone()], true);
        sync_stats_domain(&mut drive, HASH, &mut meta, &mut da).expect("A syncs");
        // Device B pulls (no PUT: desired equals remote).
        let mut db = TrackStore::new(vec![], false);
        let out_b = sync_stats_domain(&mut drive, HASH, &mut meta, &mut db).expect("B pulls");
        assert!(!out_b.pushed);
        assert_eq!(parsed_names(&db.items()), parsed_names(&[a.clone()]));

        db.push(b.clone(), true);
        sync_stats_domain(&mut drive, HASH, &mut meta, &mut db).expect("B syncs");
        let mut da2 = TrackStore::new(vec![a.clone()], false);
        sync_stats_domain(&mut drive, HASH, &mut meta, &mut da2).expect("A pulls");
        assert_eq!(
            parsed_names(&da2.items()),
            parsed_names(&[a.clone(), b.clone()])
        );
        assert_eq!(
            parsed_names(&db.items()),
            parsed_names(&[a.clone(), b.clone()])
        );
        // Sessions identical on both: 2 records.
        assert_eq!(da2.items().len(), 2);
        // Shard holds both non-legacy rows; legacy was never created.
        assert!(drive.contents_of(STATS_FILE).is_none());
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[a, b])
        );
    }

    #[test]
    fn reinstall_materializes_legacy_and_shards_with_no_put() {
        let a = stat(1, "2026-08-01", 10);
        let b = stat(2, "2026-08-02", 20);
        let c = stat(3, "2026-09-01", 30);
        let mut drive = ShardFakeDrive::new()
            .with_legacy(&[a.clone(), b.clone()], "v1")
            .with_shard("stats-2026-09.json", "id-s09", &[c.clone()], "v2");
        let mut store = TrackStore::new(vec![], false);
        let out = sync(&mut drive, &mut store).expect("reinstall pull must succeed");
        assert!(!out.pushed);
        assert!(drive.puts.is_empty(), "complete remote needs no rewrite");
        assert_eq!(parsed_names(&store.items()), parsed_names(&[a, b, c]));
    }

    #[test]
    fn rollover_writes_each_month_shard_and_never_legacy() {
        let x = stat(1, "2026-08-31", 10);
        let y = stat(2, "2026-09-01", 20);
        let mut drive = ShardFakeDrive::new();
        let mut store = TrackStore::new(vec![x.clone(), y.clone()], true);
        let out = sync(&mut drive, &mut store).expect("rollover sync must succeed");
        assert!(out.pushed);
        let mut names = drive.put_names();
        names.sort();
        assert_eq!(
            names,
            vec![
                "stats-2026-08.json".to_string(),
                "stats-2026-09.json".to_string()
            ]
        );
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[x])
        );
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-09.json")),
            parsed_names(&[y])
        );
        assert!(
            drive.contents_of(STATS_FILE).is_none(),
            "legacy never created by new clients"
        );
    }

    #[test]
    fn lifetime_invariants_match_single_ledger_baseline() {
        // 40 rows across 3 months + 2 devices, one LWW duplicate pair, one
        // legacy aggregate-style row (timestamp 0, zeroed payload).
        let mut ledger: Vec<StatsItem> = (0u32..40)
            .map(|i| {
                let day = format!("2026-{:02}-{:02}", 8 + (i % 3), (i % 28) + 1);
                stat(100 + i, &day, (i % 17 + 1) as i64)
            })
            .collect();
        let mut dup_old = stat(7, "2026-08-15", 111);
        dup_old.updated_at = Some(100);
        dup_old.device_id = Some("a".to_string());
        let mut dup_new = dup_old.clone();
        dup_new.words = Some(222);
        dup_new.updated_at = Some(200);
        let mut agg = stat(50, "2026-09-10", 0);
        agg.timestamp_ms = 0;
        agg.words = Some(500);
        agg.chars = Some(0);
        ledger.push(dup_old.clone());
        ledger.push(dup_new.clone());
        ledger.push(agg.clone());
        // Baseline: single-ledger union, canonical order.
        let mut baseline = shards::union_stats_merge(&[ledger.clone()]);
        shards::sort_stats_records(&mut baseline);

        // Sharded world: first 15 rows live in legacy, the rest sync up.
        let (legacy_rows, shard_rows) = baseline.split_at(15);
        let mut drive = ShardFakeDrive::new().with_legacy(legacy_rows, "v1");
        let mut store = TrackStore::new(baseline.clone(), true);
        let out = sync(&mut drive, &mut store).expect("bulk sync must succeed");
        assert!(out.pushed);

        // Invariant 1: store materializes exactly the baseline set.
        assert_eq!(parsed_names(&store.items()), parsed_names(&baseline));
        // Invariant 2: remote union (legacy + all shards) equals baseline.
        let mut remote_all: Vec<StatsItem> = legacy_rows.to_vec();
        for f in drive
            .files
            .iter()
            .filter(|f| shards::is_stats_shard_file_name(&f.name))
        {
            let bytes = drive.contents.get(&f.file_id).unwrap();
            remote_all.extend(StatsEnvelope::from_bytes(bytes).unwrap().entries);
        }
        let remote_merged = shards::union_stats_merge(&[remote_all]);
        assert_eq!(parsed_names(&remote_merged), parsed_names(&baseline));
        assert_eq!(remote_merged.len(), baseline.len());
        // Invariant 3: sessions = record count; sums identical.
        let sums = |rows: &[StatsItem]| {
            (
                rows.len(),
                rows.iter().map(|r| r.words.unwrap_or(0)).sum::<i64>(),
                rows.iter().map(|r| r.chars.unwrap_or(0)).sum::<i64>(),
                rows.iter().map(|r| r.duration_ms.unwrap_or(0)).sum::<i64>(),
            )
        };
        assert_eq!(sums(&store.items()), sums(&baseline));
        // Invariant 4: per-day session counts and word sums identical.
        let per_day = |rows: &[StatsItem]| {
            let mut m: std::collections::BTreeMap<String, (usize, i64)> =
                std::collections::BTreeMap::new();
            for r in rows {
                let e = m.entry(r.day.clone()).or_insert((0, 0));
                e.0 += 1;
                e.1 += r.words.unwrap_or(0);
            }
            m
        };
        assert_eq!(per_day(&store.items()), per_day(&baseline));
        // Invariant 5: timestamp multiset (weekly/activity charts) identical.
        let mut ts_store: Vec<i64> = store.items().iter().map(|r| r.timestamp_ms).collect();
        let mut ts_base: Vec<i64> = baseline.iter().map(|r| r.timestamp_ms).collect();
        ts_store.sort();
        ts_base.sort();
        assert_eq!(ts_store, ts_base);
        let _ = shard_rows;
    }

    #[test]
    fn legacy_created_later_is_absorbed_and_shards_shed_duplicates() {
        let a = stat(1, "2026-08-01", 10);
        let b = stat(2, "2026-08-02", 20);
        let c = stat(3, "2026-08-03", 30); // old-client row, legacy-only
                                           // World starts new-client-only: shards hold everything, no legacy file.
        let mut drive = ShardFakeDrive::new().with_shard(
            "stats-2026-08.json",
            "id-s08",
            &[a.clone(), b.clone()],
            "v1",
        );
        let mut store = TrackStore::new(vec![a.clone(), b.clone()], false);
        // An old client creates stats.json with its view ([b] plus its own [c]).
        drive.add_file(
            STATS_FILE,
            "id-legacy",
            "v9",
            envelope(&[b.clone(), c.clone()]),
        );
        let out = sync(&mut drive, &mut store).expect("late legacy must absorb");
        assert!(out.pushed, "shard must shed the now-legacy-identical row");
        // Shard keeps only non-legacy rows; legacy untouched.
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[a.clone()])
        );
        assert_eq!(
            drive.contents_of(STATS_FILE).unwrap(),
            envelope(&[b.clone(), c.clone()])
        );
        assert_eq!(parsed_names(&store.items()), parsed_names(&[a, b, c]));
        assert!(drive.puts.iter().all(|(n, _, _)| n != STATS_FILE));
        assert_eq!(
            drive.puts[0].2.as_deref(),
            Some("v1"),
            "shard rewrite CAS-targets the existing revision"
        );
    }

    #[test]
    fn unroutable_rows_stay_dirty_but_never_block_shards() {
        let bad = StatsItem {
            event_id: "123e4567-e89b-12d3-a456-426614174099".to_string(),
            day: "not-a-day!".to_string(),
            timestamp_ms: 1000,
            words: Some(5),
            chars: Some(25),
            duration_ms: Some(500),
            updated_at: None,
            device_id: None,
        };
        let good = stat(60, "2026-08-05", 7);
        let mut drive = ShardFakeDrive::new();
        let mut store = TrackStore::new(vec![bad.clone(), good.clone()], true);
        let out = sync(&mut drive, &mut store).expect("routable month still syncs");
        assert!(out.pushed);
        assert_eq!(drive.put_names(), vec!["stats-2026-08.json".to_string()]);
        assert_eq!(
            parsed_names(&drive.parsed_of("stats-2026-08.json")),
            parsed_names(&[good.clone()])
        );
        assert!(
            store.is_dirty(&bad.event_id),
            "unroutable row never stamped clean"
        );
        // The good row stays dirty via the tie-guard, yet the second pass
        // needs no PUT: diff-converged.
        assert!(store.is_dirty(&good.event_id));
        let out2 = sync(&mut drive, &mut store).expect("second pass must succeed");
        assert!(!out2.pushed);
        assert_eq!(drive.puts.len(), 1);
    }

    #[test]
    fn no_put_targets_the_legacy_file() {
        // Every path above routes through put_domain; assert globally that
        // the legacy name is never a PUT target in any of these scenarios
        // by exercising a mixed month + corrupt legacy + dirty rows.
        let n = stat(11, "2026-10-05", 44);
        let mut drive = ShardFakeDrive::new().with_legacy_bytes(b"garbage".to_vec(), "v3");
        let mut store = TrackStore::new(vec![n], true);
        sync(&mut drive, &mut store).expect("sync must succeed");
        assert!(!drive.put_names().contains(&STATS_FILE.to_string()));
        assert!(drive
            .put_names()
            .contains(&"stats-2026-10.json".to_string()));
    }
}
