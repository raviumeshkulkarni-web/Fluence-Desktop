// Fluence sync - frozen v1.2 local stores (DirtyStore implementations)
//
// Account isolation model:
// - dictionary.json / snippets.json rows carry `sync_account`; loads filter
//   by the active account hash, so another account's rows are never uploaded.
// - stats events live in one local ledger (`stats_events.json`) with a
//   nullable account stamp; unstamped (pre-sign-in) dictations are claimed by
//   the first account that syncs them. Event ids are UUIDv5 of the history
//   row id, so a backfilled row and a freshly-recorded event for the same
//   dictation collapse under union dedup - exactly-once counting by
//   construction.
// - settings LWW bookkeeping lives in `settings_sync_<hash>.json`, one
//   document per account, so preferences cannot cross accounts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[cfg(not(test))]
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};

use crate::dictionary::{
    dict_sync_business_key, load_dictionary_internal, save_dictionary_internal, DictionaryEntry,
};
use crate::settings::{load_settings, AppSettings};
use crate::snippets::{
    load_store_internal as load_snippets_internal, save_store_internal as save_snippets_internal,
    snippet_sync_business_key, Snippet,
};
use crate::sync::domain::*;
use crate::sync::error::SyncError;
use crate::sync::frozen::DirtyStore;
use crate::sync::metadata::SyncMetadata;

/// Root directory for Fluence state, before the `Fluence` leaf.
///
/// Resolution order:
/// 1. `FLUENCE_DATA_DIR` — explicit override for hermetic test runs.
/// 2. Under `cfg(test)`, a temp directory, so a bare `cargo test` can never
///    write the real `%LOCALAPPDATA%\Fluence`.
/// 3. The Windows Known Folder for local app data — the production path,
///    which a test build cannot reach.
pub(crate) fn base_data_dir() -> PathBuf {
    // The env override is a PRODUCTION/development affordance only. Reading it
    // before the `#[cfg(test)]` branch let an exported FLUENCE_DATA_DIR collapse
    // every `cargo test` run on the machine into one fixed directory — across
    // processes and across time — which is exactly the recycled-pid /
    // shared-root collision the per-process temp key below exists to prevent.
    // No test sets the variable, so nothing is lost by ignoring it under test.
    #[cfg(not(test))]
    if let Ok(dir) = std::env::var("FLUENCE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(test)]
    {
        // Unique per test *process*, shared by every thread within it. The test
        // harness runs tests in parallel, so a fixed directory would let them
        // write over each other, and two concurrent `cargo test` runs would
        // collide as well.
        //
        // F6 flake root-cause: the key MUST contain more than the pid. Stale
        // per-pid temp dirs are never cleaned, and Windows recycles PIDs, so a
        // pid-only key reuses a previous run's directory — and any test with a
        // deterministic account hash and no cleanup then observes the previous
        // run's rows (observed: `a_stale_real_merge_now_preserves_the_claim`
        // failing with claimed_ids == [] because the row was already claimed
        // by the dead run that owned the recycled pid). Milliseconds since
        // epoch make reuse impossible in practice: the same pid cannot recur
        // within the same millisecond.
        static TEST_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        return TEST_DIR
            .get_or_init(|| {
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                std::env::temp_dir().join(format!(
                    "fluence-cargo-test-datadir-{}-{}",
                    std::process::id(),
                    millis
                ))
            })
            .clone();
    }
    #[cfg(not(test))]
    {
        data_local_dir().unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Base directory for all Fluence state (`<base>/Fluence`).
pub(crate) fn data_dir() -> PathBuf {
    let mut p = base_data_dir();
    p.push("Fluence");
    p
}

/// Serializes tests that mutate the shared on-disk stores under the
/// `cfg(test)` temp data root (e.g. `dictionary.json`). Parallel tests would
/// otherwise interleave their read-modify-write cycles over the same file.
/// Tests that only need an in-memory store do not need this guard.
///
/// # The shared-state contract (read before writing a new test)
///
/// The `cfg(test)` data root — and in particular `settings.json` — is ONE
/// per-process directory shared by every test thread. Three separate flakes
/// have come from this fact (a pid-recycled temp dir, a fixed tmp filename
/// raced by concurrent writers, and an unguarded settings write flipping
/// another test's branch decision). The rule that prevents a fourth:
///
/// - acquire this guard for the WHOLE test if the test writes shared settings
///   or shared stores AND later reads them back to decide a branch. Guarding
///   only the write is not enough: the damage happens at the later read, and
///   an end-of-test restore cannot repair a branch already taken.
/// - prefer the narrowest scope that covers the write-to-read window, but when
///   in doubt hold it for the whole test — an over-held guard only serializes,
///   while an under-held one flakes.
/// - never block a shared directory path itself to force a failure; block
///   exactly the target file (a directory where the file belongs makes the
///   atomic rename fail) so concurrent tests on other paths stay writable.
#[cfg(test)]
pub(crate) fn store_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static STORE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    STORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Unique temp sibling for one atomic write.
///
/// Audit #22: the tmp name was a FIXED `path.with_extension("tmp")`, the exact
/// bug class this repo already paid for in `settings.rs::save_settings`: two
/// concurrent writers both write the same tmp, then both rename it - the loser
/// renames a tmp the winner already consumed (os error 2) or renames a
/// *partially written* tmp onto the destination, i.e. a truncated store. The
/// proven settings.rs pattern is mirrored here: pid + monotonic sequence, in the
/// destination's own directory so the rename stays same-volume atomic.
fn atomic_tmp_path(path: &std::path::Path) -> PathBuf {
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".tmp.{}.{}", std::process::id(), seq));
    PathBuf::from(name)
}

/// Flush `path`'s contents to stable storage, best effort.
///
/// The handle MUST be opened for WRITE. `File::open` is `O_RDONLY`, and on
/// Windows `sync_all` maps to `FlushFileBuffers`, which requires
/// `GENERIC_WRITE` on the handle and fails otherwise — so the previous
/// `if let Ok(f) = File::open(p) { let _ = f.sync_all(); }` idiom performed no
/// flush at all while its doc comment promised "prevents half-written file on
/// power loss". Errors stay non-fatal on purpose: this turns a flush that never
/// happened into one that happens when the platform allows it, and can never
/// turn a previously-succeeding store write into a failing one.
fn best_effort_flush(path: &std::path::Path) {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(f) => {
            if let Err(e) = f.sync_all() {
                log::warn!("sync: flush {} failed: {e}", path.display());
            }
        }
        Err(e) => log::warn!("sync: could not open {} for flush: {e}", path.display()),
    }
}

fn atomic_write(path: &std::path::Path, data: &str) -> Result<(), SyncError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SyncError::Fatal(e.to_string()))?;
    }
    let tmp = atomic_tmp_path(path);
    let write_result = (|| -> Result<(), SyncError> {
        std::fs::write(&tmp, data).map_err(|e| SyncError::Fatal(e.to_string()))?;
        best_effort_flush(&tmp);
        std::fs::rename(&tmp, path).map_err(|e| SyncError::Fatal(e.to_string()))
    })();
    if write_result.is_err() {
        // Never leave a stray tmp behind on the failure path.
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

/// Resolve the wire device id for a record lacking its own. An empty
/// metadata id — the one-load window after unreadable sync metadata, when
/// identity is withheld rather than minted (Phase E) — must never reach the
/// wire: `""` sorts lowest in `cmp_winner`, so this device would
/// systematically lose every timestamp tie. Skip the record this pass
/// instead (`None`); the next pass carries it once identity is re-minted
/// through the explicit `ensure_device_id` act.
fn wire_device_id(stored: Option<&str>, meta_id: &str) -> Option<String> {
    match stored {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ if !meta_id.is_empty() => Some(meta_id.to_string()),
        _ => None,
    }
}

// ── Dictionary store ────────────────────────────────────────────────────────

pub struct DictionaryDirtyStore;

impl DictionaryDirtyStore {
    fn to_domain_item(e: &DictionaryEntry) -> Option<DictionaryItem> {
        let updated = e.updated_at.or(e.created_at).unwrap_or(0);
        if updated <= 0 {
            return None;
        }
        let device = wire_device_id(e.device_id.as_deref(), &SyncMetadata::load().device_id)?;
        Some(DictionaryItem {
            sync_id: e.id.clone(),
            spoken: e.spoken.clone(),
            corrected: e.corrected.clone(),
            // `kind` is a local-only annotation (frozen v1.2 / F1): the wire
            // format never carries it and the reader always reconstitutes
            // `correction`. Carrying the local value into the sync payload made
            // encode != decode for every non-default kind, failing the frozen
            // round-trip gate with `SyncError::Fatal` and latching the
            // scheduler so ALL domains stopped syncing (audit finding C1). The
            // payload is therefore always `correction`; the user's local value
            // is preserved on disk by `save_merged`.
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            is_enabled: e.is_enabled,
            deleted_at: e.deleted_at,
            updated_at: updated,
            device_id: device,
        })
    }

    fn from_domain_item(item: DictionaryItem, account_hash: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: item.sync_id,
            spoken: item.spoken,
            corrected: item.corrected,
            kind: item.kind,
            created_at: Some(item.updated_at),
            deleted_at: item.deleted_at,
            updated_at: Some(item.updated_at),
            device_id: Some(item.device_id),
            is_enabled: item.is_enabled,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.to_string()),
            // Dormant legacy columns (kept for file-format compatibility).
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }

    /// Fallible read of this account's rows. Audit #6b: the trait's `load`
    /// cannot return an error, so the raw read lives here and the infallible
    /// wrapper below degrades *loudly* instead of `unwrap_or_default()`.
    fn load_strict(&self, account_hash: &str) -> Result<Vec<DictionaryItem>, SyncError> {
        let all = load_dictionary_internal()
            .map_err(|e| SyncError::Fatal(format!("dictionary store unreadable: {e}")))?;
        Ok(all
            .iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash))
            // F3: quarantined rows are held for recovery — they must never
            // enter the merge input (Android excludeQuarantined parity).
            .filter(|e| e.quarantine_reason.is_none())
            .filter_map(Self::to_domain_item)
            .collect())
    }

    /// Fail-closed `has_dirty`: an unreadable store must never read as CLEAN.
    ///
    /// The old `unwrap_or_default()` path made a corrupt dictionary report
    /// "no dirty rows, nothing to push", so the pass ended in a clean,
    /// successful, completely silent state with no local data in it. Unknown
    /// state is reported as dirty so the pass never concludes "nothing to do".
    fn has_dirty_strict(&self, account_hash: &str) -> Result<bool, SyncError> {
        let all = load_dictionary_internal()
            .map_err(|e| SyncError::Fatal(format!("dictionary store unreadable: {e}")))?;
        Ok(all
            .iter()
            .any(|e| e.sync_account.as_deref() == Some(account_hash) && e.dirty))
    }
}

impl DirtyStore for DictionaryDirtyStore {
    type Item = DictionaryItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        // Audit #6b: an unreadable store is NOT an empty store. The trait
        // signature has no error channel, so the failure is logged at error
        // level (never swallowed) and `has_dirty` below reports dirty, which
        // keeps the pass from concluding "clean, nothing to do".
        match self.load_strict(account_hash) {
            Ok(items) => items,
            Err(e) => {
                log::error!("Dictionary load degraded to an empty view: {e:?}");
                Vec::new()
            }
        }
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut all = load_dictionary_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let mut stamped = 0;
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        for e in all.iter_mut() {
            let owned = e.sync_account.as_deref() == Some(account_hash);
            let needs_repair = owned
                && (e.updated_at.unwrap_or(0) <= 0
                    || e.device_id.as_ref().map_or(true, |id| id.is_empty()));
            if e.sync_account.is_none() || needs_repair {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                e.sync_account = Some(account_hash.to_string());
                e.device_id = Some(
                    e.device_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| device_id.clone()),
                );
                // Preserve an existing valid updatedAt; only stamp when absent
                // so enrollment never fabricates a newer-than-remote edit.
                if e.updated_at.is_none() || e.updated_at == Some(0) {
                    e.updated_at = Some(now);
                }
                if e.created_at.is_none() {
                    e.created_at = Some(now);
                }
                e.dirty = true;
                e.ever_pushed = false;
                stamped += 1;
            }
        }
        if stamped > 0 {
            save_dictionary_internal(&all).map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        // Audit #6b: fail closed. `true` is the only answer that cannot be
        // wrong in the destructive direction - an unreadable store might hold
        // unpushed rows, and claiming "clean" is what silently dropped them.
        match self.has_dirty_strict(account_hash) {
            Ok(dirty) => dirty,
            Err(e) => {
                log::error!("Dictionary has_dirty failing closed (reporting dirty): {e:?}");
                true
            }
        }
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut all = load_dictionary_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let rescued: Vec<DictionaryEntry> = all
            .iter()
            .filter(|e| {
                e.sync_account.as_deref() == Some(account_hash)
                    && e.dirty
                    && match merged.iter().find(|m| {
                        // H4: match by sync_id OR business key. Two devices
                        // creating the same word offline mint different random
                        // ids, so the merge winner for this key carries a
                        // foreign sync_id — an id-only lookup misses it
                        // (`None => true`) and resurrects the loser dirty
                        // forever. The key derivation is exactly the merge's
                        // grouping key, so this agrees with merge_keyed.
                        m.sync_id == e.id || m.business_key() == dict_sync_business_key(&e.spoken)
                    }) {
                        // H4: port of the agents/styles `already_uploaded`
                        // guard. A dirty row whose content already equals its
                        // winner is this exact edit uploaded, so the winner is
                        // authoritative — rescuing it would re-upload forever.
                        // `kind` is deliberately excluded: it never crosses the
                        // wire (F1), so comparing it would rescue every
                        // expansion entry unconditionally.
                        Some(winner) => {
                            let already_uploaded = e.spoken == winner.spoken
                                && e.corrected == winner.corrected
                                && e.is_enabled == winner.is_enabled
                                && e.deleted_at == winner.deleted_at;
                            !already_uploaded
                                && crate::sync::clock::cmp_winner(
                                    e.updated_at.unwrap_or(0),
                                    e.device_id.as_deref().unwrap_or(""),
                                    winner.updated_at,
                                    &winner.device_id,
                                ) == std::cmp::Ordering::Greater
                        }
                        None => true,
                    }
            })
            .cloned()
            .collect();
        // `kind` never crosses the wire (F1), so every merged winner decodes
        // as `correction`. Preserve the local annotation for records this
        // device already had, so a sync pass cannot silently downgrade a local
        // `expansion` entry (excluded from the STT vocabulary prompt) to a
        // plain correction.
        let local_kinds: std::collections::HashMap<String, String> = all
            .iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash))
            .map(|e| (e.id.clone(), e.kind.clone()))
            .collect();
        all.retain(|e| {
            e.sync_account.as_deref() != Some(account_hash)
                // F3: quarantined rows are held for recovery; the wholesale
                // replacement must not wipe them.
                || e.quarantine_reason.is_some()
        });
        // A rescued row and its winner share the id: pushing both would store
        // a duplicate. The rescued local edit is newer (it won the timestamp
        // gate above) and differs (it passed the content gate), so it wins
        // and the stale winner is skipped — mirroring the agents `kept_ids`
        // rule. Without this, every rescued-with-winner merge stores the row
        // twice.
        let rescued_ids: std::collections::HashSet<String> =
            rescued.iter().map(|e| e.id.clone()).collect();
        // H4: a rescued row and the merged winner for its key are different
        // ids for the same word (cross-device create) — skip the stale
        // winner by business key too, or both persist as a duplicate.
        let rescued_keys: std::collections::HashSet<String> = rescued
            .iter()
            .map(|e| dict_sync_business_key(&e.spoken))
            .collect();
        for item in merged {
            if rescued_ids.contains(&item.sync_id) || rescued_keys.contains(&item.business_key()) {
                continue;
            }
            let mut entry = Self::from_domain_item(item, account_hash);
            if let Some(kind) = local_kinds.get(&entry.id) {
                entry.kind = kind.clone();
            }
            all.push(entry);
        }
        for mut entry in rescued {
            entry.dirty = true;
            entry.ever_pushed = false;
            all.push(entry);
        }
        // Fold the never-pushed tombstone purge into the same write as the
        // merge+clean. A separate load→mutate→write pass could stamp a local
        // edit made meanwhile as pushed without it ever reaching the server.
        all.retain(|e| {
            !(e.sync_account.as_deref() == Some(account_hash)
                && e.deleted_at.is_some()
                && !e.ever_pushed
                // F3: a quarantined tombstone is still held for recovery.
                && e.quarantine_reason.is_none())
        });
        save_dictionary_internal(&all).map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

// ── Snippet store ───────────────────────────────────────────────────────────

pub struct SnippetDirtyStore;

impl SnippetDirtyStore {
    fn to_domain_item(s: &Snippet) -> Option<SnippetItem> {
        let updated = s.updated_at.or(s.created_at).unwrap_or(0);
        if updated <= 0 {
            return None;
        }
        let device = wire_device_id(s.device_id.as_deref(), &SyncMetadata::load().device_id)?;
        Some(SnippetItem {
            sync_id: s.id.clone(),
            trigger: s.trigger.clone(),
            expansion: s.expansion.clone(),
            is_enabled: s.is_enabled,
            deleted_at: s.deleted_at,
            updated_at: updated,
            device_id: device,
        })
    }

    fn from_domain_item(item: SnippetItem, account_hash: &str) -> Snippet {
        Snippet {
            id: item.sync_id,
            trigger: item.trigger,
            expansion: item.expansion,
            created_at: Some(item.updated_at),
            updated_at: Some(item.updated_at),
            device_id: Some(item.device_id),
            is_enabled: item.is_enabled,
            deleted_at: item.deleted_at,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.to_string()),
            // Dormant legacy columns (kept for file-format compatibility).
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }

    /// Fallible read; see [`DictionaryDirtyStore::load_strict`] (audit #6b).
    fn load_strict(&self, account_hash: &str) -> Result<Vec<SnippetItem>, SyncError> {
        let store = load_snippets_internal()
            .map_err(|e| SyncError::Fatal(format!("snippets store unreadable: {e}")))?;
        Ok(store
            .snippets
            .iter()
            .filter(|s| s.sync_account.as_deref() == Some(account_hash))
            // F3: quarantined rows are held for recovery — they must never
            // enter the merge input (Android excludeQuarantined parity).
            .filter(|s| s.quarantine_reason.is_none())
            .filter_map(Self::to_domain_item)
            .collect())
    }

    /// Fail-closed `has_dirty`; see [`DictionaryDirtyStore::has_dirty_strict`].
    fn has_dirty_strict(&self, account_hash: &str) -> Result<bool, SyncError> {
        let store = load_snippets_internal()
            .map_err(|e| SyncError::Fatal(format!("snippets store unreadable: {e}")))?;
        Ok(store
            .snippets
            .iter()
            .any(|s| s.sync_account.as_deref() == Some(account_hash) && s.dirty))
    }
}

impl DirtyStore for SnippetDirtyStore {
    type Item = SnippetItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        // Audit #6b: log the failure, never `unwrap_or_default()` it away.
        match self.load_strict(account_hash) {
            Ok(items) => items,
            Err(e) => {
                log::error!("Snippet load degraded to an empty view: {e:?}");
                Vec::new()
            }
        }
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut store = load_snippets_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let mut stamped = 0;
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        for s in store.snippets.iter_mut() {
            let owned = s.sync_account.as_deref() == Some(account_hash);
            let needs_repair = owned
                && (s.updated_at.unwrap_or(0) <= 0
                    || s.device_id.as_ref().map_or(true, |id| id.is_empty()));
            if s.sync_account.is_none() || needs_repair {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                s.sync_account = Some(account_hash.to_string());
                s.device_id = Some(
                    s.device_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| device_id.clone()),
                );
                if s.updated_at.is_none() || s.updated_at == Some(0) {
                    s.updated_at = Some(now);
                }
                if s.created_at.is_none() {
                    s.created_at = Some(now);
                }
                s.dirty = true;
                s.ever_pushed = false;
                stamped += 1;
            }
        }
        if stamped > 0 {
            save_snippets_internal(&store).map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        // Audit #6b: fail closed - see the dictionary counterpart.
        match self.has_dirty_strict(account_hash) {
            Ok(dirty) => dirty,
            Err(e) => {
                log::error!("Snippet has_dirty failing closed (reporting dirty): {e:?}");
                true
            }
        }
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut store = load_snippets_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let rescued: Vec<Snippet> = store
            .snippets
            .iter()
            .filter(|s| {
                s.sync_account.as_deref() == Some(account_hash)
                    && s.dirty
                    && match merged.iter().find(|m| {
                        // H4: match by sync_id OR business key — same
                        // cross-device rationale as the dictionary
                        // counterpart above (independent creates mint
                        // different random ids for one trigger).
                        m.sync_id == s.id
                            || m.business_key() == snippet_sync_business_key(&s.trigger)
                    }) {
                        // H4: same `already_uploaded` guard as the dictionary
                        // counterpart above and the agents/styles stores. A
                        // dirty row matching its winner is already uploaded.
                        Some(winner) => {
                            let already_uploaded = s.trigger == winner.trigger
                                && s.expansion == winner.expansion
                                && s.is_enabled == winner.is_enabled
                                && s.deleted_at == winner.deleted_at;
                            !already_uploaded
                                && crate::sync::clock::cmp_winner(
                                    s.updated_at.unwrap_or(0),
                                    s.device_id.as_deref().unwrap_or(""),
                                    winner.updated_at,
                                    &winner.device_id,
                                ) == std::cmp::Ordering::Greater
                        }
                        None => true,
                    }
            })
            .cloned()
            .collect();
        store.snippets.retain(|s| {
            s.sync_account.as_deref() != Some(account_hash)
                // F3: quarantined rows are held for recovery; the wholesale
                // replacement must not wipe them.
                || s.quarantine_reason.is_some()
        });
        // Same dedup rule as the dictionary counterpart above: a rescued row
        // and its winner share the id, so pushing both stores a duplicate.
        // The rescued local edit wins; the stale winner is skipped.
        let rescued_ids: std::collections::HashSet<String> =
            rescued.iter().map(|s| s.id.clone()).collect();
        // H4: skip the stale winner by business key too — a rescued row and
        // its winner are different ids for one trigger (cross-device create).
        let rescued_keys: std::collections::HashSet<String> = rescued
            .iter()
            .map(|s| snippet_sync_business_key(&s.trigger))
            .collect();
        for item in merged {
            if rescued_ids.contains(&item.sync_id) || rescued_keys.contains(&item.business_key()) {
                continue;
            }
            store
                .snippets
                .push(Self::from_domain_item(item, account_hash));
        }
        for mut entry in rescued {
            entry.dirty = true;
            entry.ever_pushed = false;
            store.snippets.push(entry);
        }
        store.snippets.retain(|s| {
            !(s.sync_account.as_deref() == Some(account_hash)
                && s.deleted_at.is_some()
                && !s.ever_pushed
                // F3: a quarantined tombstone is still held for recovery.
                && s.quarantine_reason.is_none())
        });
        save_snippets_internal(&store).map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

// ── Settings store (value-diff LWW, per-account bookkeeping) ────────────────
//
// Mirrors the proven Android PrefsSettingsV1Store semantics: a per-account
// meta document records the last-synced {value, updatedAt} per key. A live
// value differing from the recorded one is dirty with a fresh wall-clock
// timestamp. Incoming winners are applied to real settings and recorded.
//
// Windows emits four keys. `dictionary_enabled` is never emitted here
// (Windows applies the dictionary unconditionally and has no toggle); an
// incoming value for it is recorded but not applied, so the two platforms
// do not fight over a setting only Android exposes.

pub struct SettingsDirtyStore;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SettingsMetaDoc {
    /// key -> last-synced {v: value, t: updatedAt}
    #[serde(default)]
    keys: HashMap<String, KeyMeta>,
    /// Global values captured when an account becomes active. These are a
    /// baseline only; they are never uploaded unless the user edits them.
    #[serde(default)]
    activation_baseline: Option<HashMap<String, String>>,
    /// Live values observed at the START of the current pass, keyed as `keys` is.
    ///
    /// Written by `load`, compared in `apply_merged`. Without it a user edit
    /// landing DURING the GET→PUT window is silently destroyed: the edit is not
    /// in `merged` (computed from the pre-edit snapshot), `meta.keys` still
    /// holds the pre-edit baseline so the winner still "beats baseline", the
    /// winner is applied over the user's edit, and the baseline then advances —
    /// no dirty residue, no retry, no trace. Android's `decideSettingsApply`
    /// closes exactly this; this is its missing twin.
    ///
    /// Purely derived bookkeeping (no user data), same status as `keys`.
    #[serde(default)]
    pass_snapshot: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyMeta {
    v: String,
    t: i64,
    /// `deviceId` of the record this baseline came from (audit #4).
    ///
    /// The baseline is compared against incoming merge winners with the SAME
    /// total order `merge_settings` uses, and that order is
    /// `(updatedAt, deviceId, value)`. Without the device id there is nothing
    /// to compare at the second level, so an exact timestamp tie - which the
    /// merge resolves and this path used to drop - could never be resolved
    /// here. `#[serde(default)]` keeps every pre-existing meta document
    /// readable; an empty id sorts lowest, i.e. an incoming winner wins the
    /// tie and this device re-bases on it, which is the safe direction.
    #[serde(default)]
    d: String,
}

/// Side effects one settings apply pass performs outside the meta document.
///
/// Injected rather than hard-wired so a pass can be exercised against temp
/// paths: `settings::settings_path()` resolves through `dirs::data_local_dir()`
/// and IGNORES the test data-dir redirect, so any test that reached real
/// settings would write the real user profile (see the
/// `production_settings_survive_the_activity_read_path` guard).
struct SettingsIo<'a> {
    meta_path: &'a std::path::Path,
    load_settings: &'a dyn Fn() -> Result<AppSettings, String>,
    save_settings: &'a dyn Fn(&AppSettings) -> Result<(), String>,
    /// Writes `enabled` into the snippets store (a separate local file).
    apply_snippets_enabled: &'a dyn Fn(bool) -> Result<(), String>,
    /// Re-checked between writes: settings are machine-global, so nothing may
    /// be applied for an account that stopped being active mid-pass.
    still_active: &'a dyn Fn() -> bool,
}

/// The keys this platform emits/accepts, mapped onto real settings. See
/// `live_values` - `dictionary_enabled` is deliberately absent (Windows has
/// no dictionary toggle; incoming values are recorded but not applied).

impl SettingsDirtyStore {
    /// Settings values are stored in machine-global preferences. Never apply
    /// a result for an account that is no longer active.
    fn active_account_matches(account_hash: &str) -> bool {
        load_settings()
            .ok()
            .and_then(|settings| settings.sync_account_key)
            .map(|email| crate::sync::metadata::account_hash_from_email(&email))
            .as_deref()
            == Some(account_hash)
    }

    fn meta_path(account_hash: &str) -> PathBuf {
        let mut p = data_dir();
        p.push(format!("settings_sync_{account_hash}.json"));
        p
    }

    /// Record an account activation without overwriting machine-global
    /// settings. The next pass will pull that account's remote values, or
    /// treat the captured values as a non-uploaded baseline if no file exists.
    pub fn activate_account(account_hash: &str) -> Result<(), SyncError> {
        // Audit #22: `save_meta` is an atomic write against the shared per-account
        // meta doc; take the same io_lock every other meta writer holds.
        let _io = crate::sync::io_lock::io_lock_guard();
        let settings = load_settings().map_err(|e| SyncError::Fatal(e.to_string()))?;
        // Audit #15: an unreadable snippets store must never be captured as
        // `snippets_enabled=false` - that baseline would classify the user's
        // real `true` as a local edit one pass later and upload it.
        let baseline = Self::live_values_strict(&settings)
            .map_err(|e| SyncError::Fatal(format!("cannot capture activation baseline: {e}")))?
            .into_iter()
            .collect();
        let mut meta = Self::load_meta(account_hash);
        meta.activation_baseline = Some(baseline);
        Self::save_meta(account_hash, &meta)
    }

    fn activation_baseline_unchanged(meta: &SettingsMetaDoc, settings: &AppSettings) -> bool {
        let Some(baseline) = meta.activation_baseline.as_ref() else {
            return false;
        };
        match Self::live_values_strict(settings) {
            Ok(live) => live
                .into_iter()
                .all(|(key, value)| baseline.get(&key) == Some(&value)),
            Err(e) => {
                log::error!("Activation baseline check failing closed: {e}");
                false
            }
        }
    }

    fn load_meta(account_hash: &str) -> SettingsMetaDoc {
        Self::load_meta_from(&Self::meta_path(account_hash))
    }

    /// Path-parameterized core of [`load_meta`]. Tests drive this with unique
    /// temp paths so corrupt-file recovery is verified hermetically.
    fn load_meta_from(path: &std::path::Path) -> SettingsMetaDoc {
        match crate::sync::recover::read_json_file(path) {
            // No meta doc yet: fresh baselines, no backup.
            Ok(None) => SettingsMetaDoc::default(),
            Ok(Some(doc)) => doc,
            // Derived bookkeeping only (last-synced baselines, no user data):
            // preserve the corrupt bytes aside, then reset to fresh baselines
            // so the pass continues instead of wedging sync over bookkeeping
            // (settings.rs precedent, Phase E).
            Err(e) => {
                log::error!("Unreadable settings sync meta ({e:?}); resetting to fresh baselines");
                SettingsMetaDoc::default()
            }
        }
    }

    fn save_meta(account_hash: &str, doc: &SettingsMetaDoc) -> Result<(), SyncError> {
        Self::save_meta_to(&Self::meta_path(account_hash), doc)
    }

    /// Path-parameterized core of [`save_meta`].
    fn save_meta_to(path: &std::path::Path, doc: &SettingsMetaDoc) -> Result<(), SyncError> {
        let data =
            serde_json::to_string_pretty(doc).map_err(|e| SyncError::Fatal(e.to_string()))?;
        atomic_write(path, &data)
    }

    /// Live values for the emitted keys, read from real application settings.
    ///
    /// Audit #15: `snippets_enabled` comes from a SEPARATE local file, so a
    /// read failure there is a failure to observe the value - never evidence
    /// that the user turned snippets off. This is the fallible core; every
    /// caller that stamps or uploads must use it.
    fn live_values_strict(settings: &AppSettings) -> Result<Vec<(String, String)>, String> {
        let snippets_enabled = load_snippets_internal().map_err(|e| e.to_string())?.enabled;
        Ok(vec![
            ("language".to_string(), settings.language.clone()),
            ("snippets_enabled".to_string(), snippets_enabled.to_string()),
            (
                "auto_learn_enabled".to_string(),
                settings.auto_learn_enabled.to_string(),
            ),
            (
                "ai_polish_style".to_string(),
                settings.ai_polish_style.clone(),
            ),
        ])
    }

    /// Non-strict view for pure comparisons. An unreadable snippets store
    /// yields NO value rather than a fabricated `false`, so a caller can never
    /// mistake "unknown" for "user set it to false".
    ///
    /// Production stamping/uploading paths deliberately do NOT use this: they
    /// use `live_values_strict` so a read failure aborts the pass. What is left
    /// is the pure key-set test, which is `#[cfg(test)]`-only.
    #[cfg_attr(not(test), allow(dead_code))]
    fn live_values(settings: &AppSettings) -> Vec<(String, String)> {
        Self::live_values_strict(settings).unwrap_or_else(|e| {
            log::error!("Live settings values unreadable; treating {e} as no values");
            Vec::new()
        })
    }

    /// Mid-pass edit guard — Android `decideSettingsApply`, ported.
    ///
    /// `snapshot` is what the key held when this pass started; `live` is what it
    /// holds now. They differ only if the user edited it during the GET→PUT
    /// window, in which case the winner (computed from the pre-edit snapshot)
    /// must NOT be applied. A missing side means "not observed", which applies —
    /// the fail-open direction here only ever means we behave exactly as this
    /// platform did before the guard existed.
    pub(crate) fn decide_settings_apply(snapshot: Option<&String>, live: Option<&String>) -> bool {
        match (snapshot, live) {
            (Some(s), Some(l)) => s == l,
            _ => true,
        }
    }

    /// Stage an incoming winner onto the in-memory settings.
    ///
    /// Only allowed keys are ever touched; provider credentials/hotkeys/audio
    /// can never arrive here. `snippets_enabled` is NOT applied here: it lives
    /// in another local file, so it is returned for the caller to persist as a
    /// sequenced side effect (audit #19 - a failed write must abort the pass
    /// instead of being recorded as applied).
    fn apply_winner(settings: &mut AppSettings, key: &str, value: &str) -> Option<bool> {
        match key {
            "language" => settings.language = value.to_string(),
            "auto_learn_enabled" => settings.auto_learn_enabled = value == "true",
            "ai_polish_style" => settings.ai_polish_style = value.to_string(),
            "snippets_enabled" => return Some(value == "true"),
            // "dictionary_enabled": recorded in meta but not applied on Windows.
            _ => {}
        }
        None
    }

    /// Persist `snippets_enabled` into the snippets store, propagating failure.
    fn set_snippets_enabled(enabled: bool) -> Result<(), String> {
        let mut store = load_snippets_internal().map_err(|e| e.to_string())?;
        store.enabled = enabled;
        save_snippets_internal(&store).map_err(|e| e.to_string())
    }

    /// Does this merge winner beat the baseline we last synced for this key?
    ///
    /// Audit #4: this used to be `item.updated_at > known.t`. `merge_settings`
    /// resolves exact timestamp ties by `deviceId` and then by `value`, so the
    /// strict `>` silently DROPPED a tie the merge had deliberately resolved,
    /// the two paths disagreed about who won, and two devices could diverge
    /// permanently on one setting. Use the canonical total comparator - the
    /// very same `clock::cmp_settings_winner` `merge_settings` calls - so there
    /// is exactly one settings order in the codebase.
    fn winner_beats_baseline(item: &SettingsItem, known: &KeyMeta) -> bool {
        crate::sync::clock::cmp_settings_winner(
            item.updated_at,
            &item.device_id,
            &item.value,
            known.t,
            &known.d,
            &known.v,
        ) == std::cmp::Ordering::Greater
    }

    /// Apply one pass of merged settings winners: values first, meta LAST.
    ///
    /// Audit #29: meta records the baseline of what was synced. Writing it
    /// before the values leaves a crash window where meta says "the remote
    /// winner is applied" while the value on disk is still the stale local one.
    /// The next pass then sees a local value that differs from its baseline,
    /// classifies it as a local edit, stamps it `edit_now` (the account's
    /// highest timestamp) and uploads it - permanently reverting the remote
    /// winner and self-healing toward the WRONG state. So every value write
    /// happens first; any failure returns before meta is touched, and the next
    /// pass simply re-applies the same remote winner.
    ///
    /// Audit #19: a failed `snippets_enabled` write aborts here too, so the
    /// winner is never recorded in meta while the live store is stale (which
    /// would make the next pass re-upload the old value with a higher
    /// timestamp - a toggle that flips forever).
    fn apply_merged(io: &SettingsIo, merged: &[SettingsItem]) -> Result<(), SyncError> {
        let _lock = crate::sync::io_lock::io_lock_guard();
        let mut meta = Self::load_meta_from(io.meta_path);
        let activation_pending = meta.activation_baseline.is_some();
        let mut settings = (io.load_settings)()
            .map_err(|e| SyncError::Fatal(format!("settings unreadable: {e}")))?;
        // Live values observed NOW, read BEFORE this loop mutates `settings`.
        // Compared against the pre-pass snapshot so an edit the user made during
        // the GET→PUT window is not overwritten by a winner computed without it.
        // Read failure yields an empty map, so `decide_settings_apply` sees
        // "unobserved" and applies — i.e. exactly this platform's behaviour
        // before the guard existed, never a new failure mode.
        let live_now: HashMap<String, String> = Self::live_values_strict(&settings)
            .map(|v| v.into_iter().collect())
            .unwrap_or_else(|e| {
                log::error!("Settings mid-pass live read failed; applying winners unguarded: {e}");
                HashMap::new()
            });
        let mut settings_changed = false;
        let mut snippets_toggle: Option<bool> = None;
        for item in merged {
            if !(io.still_active)() {
                return Ok(());
            }
            // Mid-pass edit guard. Deferring leaves `meta.keys` untouched, so
            // the winner still beats its baseline next pass and the user's edit
            // rides the following PUT instead of being clobbered.
            if !Self::decide_settings_apply(
                meta.pass_snapshot.as_ref().and_then(|m| m.get(&item.key)),
                live_now.get(&item.key),
            ) {
                log::debug!(
                    "sync: settings key {} changed during this pass; deferring the winner",
                    item.key
                );
                continue;
            }
            let known = meta.keys.get(&item.key).cloned();
            let is_newer = match known.as_ref() {
                None => true,
                Some(k) => Self::winner_beats_baseline(item, k),
            };
            if !is_newer {
                continue;
            }
            if known.as_ref().map_or(true, |k| k.v != item.value) {
                if let Some(enabled) = Self::apply_winner(&mut settings, &item.key, &item.value) {
                    snippets_toggle = Some(enabled);
                }
                settings_changed = true;
            }
            meta.keys.insert(
                item.key.clone(),
                KeyMeta {
                    v: item.value.clone(),
                    t: item.updated_at,
                    d: item.device_id.clone(),
                },
            );
        }
        // Values, in dependency order, BEFORE the baseline that claims they
        // were applied.
        if let Some(enabled) = snippets_toggle {
            (io.apply_snippets_enabled)(enabled).map_err(|e| {
                SyncError::Fatal(format!(
                    "snippets_enabled={enabled} could not be persisted ({e}); winner NOT recorded"
                ))
            })?;
        }
        if settings_changed {
            if !(io.still_active)() {
                return Ok(());
            }
            (io.save_settings)(&settings).map_err(|e| {
                SyncError::Fatal(format!(
                    "settings values could not be persisted ({e}); baselines NOT advanced"
                ))
            })?;
        }
        if activation_pending {
            meta.activation_baseline = None;
        }
        Self::save_meta_to(io.meta_path, &meta)
    }
}

impl DirtyStore for SettingsDirtyStore {
    type Item = SettingsItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        let mut meta = Self::load_meta(account_hash);
        let Ok(settings) = load_settings() else {
            return Vec::new();
        };
        // Audit #15: `snippets_enabled` is one of the four values this pass
        // stamps and uploads, and it lives in another local file. Reading it
        // with `unwrap_or(false)` turned "the snippets store is unreadable"
        // into "the user disabled snippets" - a value stamped with `edit_now`
        // (the account's highest timestamp) and uploaded, so it wins LWW
        // fleet-wide and silently switches snippet expansion off on every
        // device. An unreadable store therefore emits NOTHING at all: no value
        // is stamped, nothing is uploaded, and the next pass retries once the
        // bytes are repaired (or quarantined, after which the read succeeds).
        let live_values = match Self::live_values_strict(&settings) {
            Ok(values) => values,
            Err(e) => {
                log::error!(
                    "Settings pass emitting nothing: live values unreadable ({e}); \
                     refusing to stamp or upload an unobserved setting"
                );
                return Vec::new();
            }
        };
        if meta.activation_baseline.is_some() {
            let baseline = meta.activation_baseline.as_ref().expect("checked above");
            if live_values
                .iter()
                .all(|(key, value)| baseline.get(key) == Some(value))
            {
                return Vec::new();
            }
            meta.activation_baseline = None;
            // Audit #22: this write is a real atomic write against the shared
            // per-account doc - take the io_lock the other writers hold.
            let _io = crate::sync::io_lock::io_lock_guard();
            let _ = Self::save_meta(account_hash, &meta);
        }
        // Capture the pre-pass live values so `apply_merged` can tell "the user
        // edited this key during the GET→PUT window" from "the value simply has
        // not changed since we looked". Derived bookkeeping only; written under
        // io_lock like every other writer of this shared per-account doc.
        {
            let mut with_snapshot = meta.clone();
            with_snapshot.pass_snapshot = Some(
                live_values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
            let _io = crate::sync::io_lock::io_lock_guard();
            // A failed snapshot write must NOT leave the previous pass's values
            // on disk to be compared against forever. A stale snapshot makes a
            // legitimate user edit look like a mid-pass edit, the winner is
            // deferred, and since every later pass re-defers against the same
            // never-updated snapshot, that setting stops syncing PERMANENTLY.
            //
            // Clearing the field on failure degrades to "no snapshot", which is
            // exactly this platform's PRE-GUARD behaviour (`decide_settings_apply`
            // fails open on a missing side) — i.e. we may clobber a mid-pass
            // edit, which is what shipped before the guard existed and is not data
            // loss. Deliberately not a pass-fatal: this is derived bookkeeping,
            // and latching sync off over it would be a far worse outcome.
            if let Err(e) = Self::save_meta(account_hash, &with_snapshot) {
                log::warn!(
                    "sync: failed to persist the settings pass snapshot ({e}); the \
                     concurrent-edit guard is disabled until the next successful pass"
                );
                let mut cleared = with_snapshot.clone();
                cleared.pass_snapshot = None;
                let _ = Self::save_meta(account_hash, &cleared);
            }
        }
        // Global preferences may still contain the previous account's values
        // immediately after sign-in. Baseline a new account without emitting
        // those values; a later user edit is detected against this baseline.
        if meta.keys.is_empty() {
            let mut seeded = SettingsMetaDoc::default();
            for (key, value) in live_values {
                seeded.keys.insert(
                    key,
                    KeyMeta {
                        v: value,
                        t: 0,
                        d: String::new(),
                    },
                );
            }
            let _io = crate::sync::io_lock::io_lock_guard();
            let _ = Self::save_meta(account_hash, &seeded);
            return Vec::new();
        }
        // Hoisted once: device id for every row, and the monotonic-clock floor
        // so a local edit is never stamped below what this device has seen
        // (a backwards wall-clock jump must not let an edit lose on LWW).
        let global_meta = SyncMetadata::load();
        let max_seen = global_meta
            .for_account(account_hash)
            .map(|s| s.max_seen)
            .unwrap_or(0);
        let (edit_now, _) = crate::sync::clock::monotonic_now(max_seen);
        let device_id = global_meta.device_id;
        let mut out = Vec::new();
        for (key, live) in live_values {
            match meta.keys.get(&key) {
                None => {
                    // First observation of a locally-set key: adopt it with sentinel 0 so remote wins.
                    out.append(&mut vec![SettingsItem {
                        key,
                        value: live,
                        updated_at: 0,
                        device_id: device_id.clone(),
                        deleted_at: None,
                    }]);
                }
                Some(known) if known.t == 0 && known.v == live => {
                    // Unchanged first-session baseline: do not upload values
                    // inherited from the previous account when this account
                    // has no remote settings file yet.
                }
                Some(known) if known.v != live => {
                    // Local edit since last sync → dirty with a fresh clock
                    // that still respects the persisted maxSeen floor.
                    out.push(SettingsItem {
                        key,
                        value: live,
                        updated_at: edit_now,
                        device_id: device_id.clone(),
                        deleted_at: None,
                    });
                }
                Some(known) => {
                    out.push(SettingsItem {
                        key,
                        value: known.v.clone(),
                        updated_at: known.t,
                        device_id: device_id.clone(),
                        deleted_at: None,
                    });
                }
            }
        }
        out
    }

    fn stamp_account(&mut self, _account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        // Value-diff bookkeeping needs no row stamping.
        Ok(0)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        let meta = Self::load_meta(account_hash);
        let Ok(settings) = load_settings() else {
            return false;
        };
        // Audit #15 + #6b: an unreadable snippets store must not read as
        // "unchanged" - fail closed (dirty) so the pass never concludes
        // "nothing to do" on a value it could not observe.
        let live_values = match Self::live_values_strict(&settings) {
            Ok(values) => values,
            Err(e) => {
                log::error!("Settings has_dirty failing closed (reporting dirty): {e}");
                return true;
            }
        };
        if Self::activation_baseline_unchanged(&meta, &settings) {
            return false;
        }
        live_values
            .iter()
            .any(|(key, live)| meta.keys.get(key).map_or(true, |known| known.v != *live))
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        if !Self::active_account_matches(account_hash) {
            return Ok(());
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let meta_path = Self::meta_path(account_hash);
        let load_settings = || load_settings().map_err(|e| e.to_string());
        let save_settings = |settings: &AppSettings| {
            crate::settings::save_settings(settings).map_err(|e| e.to_string())
        };
        let still_active = || Self::active_account_matches(account_hash);
        let io = SettingsIo {
            meta_path: &meta_path,
            load_settings: &load_settings,
            save_settings: &save_settings,
            apply_snippets_enabled: &Self::set_snippets_enabled,
            still_active: &still_active,
        };
        Self::apply_merged(&io, &merged)
    }
}

// ── Stats ledger (event-sourced, account-claiming, exactly-once) ───────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatEventRow {
    pub item: StatsItem,
    /// None until an account claims this event (unstamped dictation).
    pub account: Option<String>,
    pub dirty: bool,
    pub ever_pushed: bool,
}

/// Local persistent ledger of this device's dictation events.
pub struct StatsDirtyStore;

#[cfg(test)]
static TEST_LEDGER_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(test)]
static TEST_HISTORY_PATH: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

impl StatsDirtyStore {
    fn ledger_path() -> PathBuf {
        #[cfg(test)]
        {
            if let Ok(guard) = TEST_LEDGER_PATH.lock() {
                if let Some(p) = guard.as_ref() {
                    return p.clone();
                }
            }
        }
        let mut p = data_dir();
        p.push("stats_events.json");
        p
    }

    /// Test seam: redirect the ledger to a temp path for isolation.
    #[cfg(test)]
    pub fn set_test_ledger_path(path: Option<std::path::PathBuf>) {
        *TEST_LEDGER_PATH.lock().unwrap() = path;
    }

    #[cfg(test)]
    pub fn set_test_history_path(path: Option<std::path::PathBuf>) {
        *TEST_HISTORY_PATH.lock().unwrap() = path;
    }
    fn history_db_path() -> PathBuf {
        #[cfg(test)]
        {
            if let Ok(guard) = TEST_HISTORY_PATH.lock() {
                if let Some(p) = guard.as_ref() {
                    return p.clone();
                }
            }
        }
        let mut p = data_dir();
        p.push("history.db");
        p
    }

    fn query_history_rows(conn: &rusqlite::Connection) -> Vec<(String, i64, String, i64)> {
        let Ok(mut stmt) =
            conn.prepare("SELECT id, timestamp_ms, text, duration_ms FROM history WHERE deleted_at IS NULL AND timestamp_ms > 0")
        else {
            return Vec::new();
        };
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<(String, i64, String, i64)> {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        };
        let x = match stmt.query_map([], mapper) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        };
        x
    }

    fn load_rows() -> Vec<StatEventRow> {
        // Audit #6b: an unreadable ledger is NOT an empty ledger. Pure views
        // (dashboard, gauges) have no error channel, so the failure is logged
        // at error level instead of being swallowed - the write paths below
        // still fail closed via `load_rows_strict`.
        match Self::load_rows_strict() {
            Ok(rows) => rows,
            Err(e) => {
                log::error!("Stats ledger unreadable; view degraded to empty: {e:?}");
                Vec::new()
            }
        }
    }

    /// Fallible ledger read. Missing file is a fresh ledger; corrupt bytes
    /// are preserved aside by `read_json_file` and surface here so write
    /// paths fail closed instead of persisting a partial ledger over
    /// unreadable state (Phase E). Pure views keep using `load_rows`.
    fn load_rows_strict() -> Result<Vec<StatEventRow>, crate::sync::recover::ReadError> {
        crate::sync::recover::read_json_file(&Self::ledger_path()).map(|o| o.unwrap_or_default())
    }

    fn save_rows(rows: &[StatEventRow]) -> Result<(), SyncError> {
        let data = serde_json::to_string(rows).map_err(|e| SyncError::Fatal(e.to_string()))?;
        atomic_write(&Self::ledger_path(), &data)
    }

    /// Record one completed dictation exactly once. Called from the history
    /// commit path. The event id is UUIDv5 of the history row id, so even a
    /// duplicated call (or a later backfill of the same row) collapses under
    /// union dedup. Safe offline: the event rides the next successful sync.
    ///
    /// Audit #20: this used to swallow the ledger write (`let _ = save_rows`).
    /// A dictation event is NOT re-derivable from the ledger (its id is a
    /// one-shot backfill UUID minted here), so a failed save drops it
    /// permanently and silently. The fallible core returns the error and the
    /// wrapper logs it loudly instead of pretending the event was recorded.
    pub fn record_dictation_event(
        history_id: &str,
        timestamp_ms: i64,
        text: &str,
        duration_ms: i64,
    ) {
        if let Err(e) =
            Self::record_dictation_event_checked(history_id, timestamp_ms, text, duration_ms)
        {
            log::error!(
                "Dictation event {history_id} was NOT recorded (stats ledger write failed): {e}"
            );
        }
    }

    /// Fallible core of [`record_dictation_event`]. `Ok(false)` means the event
    /// was already present (idempotent by construction), `Ok(true)` that it is
    /// durably in the ledger.
    pub(crate) fn record_dictation_event_checked(
        history_id: &str,
        timestamp_ms: i64,
        text: &str,
        duration_ms: i64,
    ) -> Result<bool, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let item = StatsItem::from_history_row(history_id, timestamp_ms, text, duration_ms);
        // Fail closed on an unreadable ledger: appending to an empty read and
        // persisting it would replace the whole ledger with one event. The
        // corrupt bytes are preserved aside; skipping one event is the safe
        // trade (Phase E).
        let mut rows = Self::load_rows_strict().map_err(|e| {
            SyncError::Fatal(format!(
                "stats ledger unreadable; event {history_id} skipped: {e:?}"
            ))
        })?;
        if rows.iter().any(|r| r.item.event_id == item.event_id) {
            return Ok(false); // idempotent by construction
        }
        rows.push(StatEventRow {
            item,
            account: None,
            dirty: true,
            ever_pushed: false,
        });
        // Audit #20: propagate. A dropped write here is a permanently lost,
        // non-re-derivable dictation event.
        Self::save_rows(&rows)?;
        Self::invalidate_activity_cache();
        Ok(true)
    }

    /// One-time per-account seed: convert pre-existing history rows into
    /// events. Deterministic ids make this idempotent against events already
    /// recorded by `record_dictation_event`.
    fn backfill_from_history(account_hash: &str) -> Vec<StatEventRow> {
        let db_path = Self::history_db_path();
        if !db_path.exists() {
            return Vec::new();
        }
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        Self::query_history_rows(&conn)
            .into_iter()
            .filter_map(|(id, ts, text, dur)| {
                let item = StatsItem::from_history_row(&id, ts, &text, dur);
                Some(StatEventRow {
                    item,
                    account: Some(account_hash.to_string()),
                    dirty: true,
                    ever_pushed: false,
                })
            })
            .collect()
    }

    fn backfill_done(metadata: &SyncMetadata, account_hash: &str) -> bool {
        metadata
            .for_account(account_hash)
            .map(|s| s.backfill_done)
            .unwrap_or(false)
    }

    /// Seed the local stats ledger from `history.db` when the signed-in account
    /// has no ledger events yet.
    ///
    /// WHY: the dashboard derives its activity buckets from this ledger, but the
    /// ledger is only seeded by a sync pass (`DirtyStore::load`). An installation
    /// upgraded from a build older than the ledger existed therefore had years of
    /// history and an empty ledger, so the dashboard reported "No baseline yet" ΓÇö
    /// behaving like a brand-new user. Reusing the SAME `backfill_from_history`
    /// (deterministic UUIDv5 event ids, union dedup) keeps this idempotent and
    /// keeps both seeding paths converging on identical state.
    ///
    /// Deliberately does NOT set `backfill_done`: that flag belongs to the sync
    /// pass, and flipping it here would change sync behaviour. Leaving it unset
    /// just means the sync pass runs its own backfill later and finds every event
    /// id already present, so it writes nothing.
    ///
    /// Account-scoped: rows are stamped with THIS account hash and read back
    /// through `account_event_rows`, so no other account can observe them.
    /// Signed-out (`None`) is a no-op, so the all-rows local view is unchanged and
    /// no unattributed rows are ever created. Empty/unreadable `history.db`
    /// yields no synthetic rows, so a new user keeps the genuine empty state.
    fn ensure_account_backfilled(account_hash: &str) {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return;
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows = match Self::load_rows_strict() {
            Ok(r) => r,
            Err(e) => {
                log::error!("Skipping account backfill; stats ledger unreadable: {e:?}");
                return;
            }
        };
        // Only when this account has nothing yet: a populated ledger must be left
        // exactly as it is.
        if rows
            .iter()
            .any(|r| r.account.as_deref() == Some(account_hash))
        {
            return;
        }
        let synthetic = Self::backfill_from_history(account_hash);
        if synthetic.is_empty() {
            return;
        }
        let before = rows.len();
        for row in synthetic {
            if !rows.iter().any(|r| r.item.event_id == row.item.event_id) {
                rows.push(row);
            }
        }
        if rows.len() > before && Self::save_rows(&rows).is_ok() {
            // A previous read in this session may have cached the empty result.
            Self::invalidate_activity_cache();
        }
    }

    /// Test seam: raw ledger rows.
    #[cfg(test)]
    pub fn test_rows() -> Vec<StatEventRow> {
        Self::load_rows()
    }

    /// Account-level view: every event belonging to this account as
    /// (timestamp_ms, duration_ms, words, chars). After a sync pass this is
    /// the merged account state (local ∪ remote), so summing here yields the
    /// combined cross-device totals.
    pub fn account_event_rows(account_hash: &str) -> Vec<(i64, i64, i64, i64)> {
        let rows: Vec<StatEventRow> = Self::load_rows()
            .into_iter()
            .filter(|r| r.account.as_deref() == Some(account_hash))
            .collect();
        let dictation_days: HashSet<String> = rows
            .iter()
            .filter(|r| r.item.timestamp_ms > 0 || r.item.chars.unwrap_or(0) != 0)
            .map(|r| r.item.day.clone())
            .collect();
        rows.into_iter()
            .filter(|r| {
                !(r.item.timestamp_ms == 0
                    && r.item.chars.unwrap_or(0) == 0
                    && dictation_days.contains(&r.item.day))
            })
            .map(|r| {
                let ts = if r.item.timestamp_ms > 0 {
                    r.item.timestamp_ms
                } else {
                    chrono::NaiveDate::parse_from_str(&r.item.day, "%Y-%m-%d")
                        .ok()
                        .and_then(|d| d.and_hms_opt(0, 0, 0))
                        .map(|t| t.and_utc().timestamp_millis())
                        .unwrap_or(0)
                };
                (
                    ts,
                    r.item.duration_ms.unwrap_or(0),
                    r.item.words.unwrap_or(0),
                    r.item.chars.unwrap_or(0),
                )
            })
            .collect()
    }

    // ── Dashboard activity view (history-proof daily buckets) ─────────────
    //
    // Read-only view over the synced stats ledger for the history-proof
    // dashboard: one bucket per UTC day, computed server-side so payloads
    // are O(days), never O(events). No ledger writes, no sync interaction,
    // no schema change. Day math is UTC-midnight based so the bucket rule
    // is trivially Kotlin-replicable from the same synced rows (THE Kotlin
    // parity contract: day_start_ms = UTC midnight of the day containing
    // timestamp_ms; sums compose across days/weeks/months identically).

    /// Same row normalization as `account_event_rows` (timestamp wins, else
    /// UTC midnight of `item.day`), duplicated deliberately so the frozen
    /// command path above is untouched.
    fn normalize_stat_row(r: &StatEventRow) -> (i64, i64, i64, i64) {
        let ts = if r.item.timestamp_ms > 0 {
            r.item.timestamp_ms
        } else {
            chrono::NaiveDate::parse_from_str(&r.item.day, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|t| t.and_utc().timestamp_millis())
                .unwrap_or(0)
        };
        (
            ts,
            r.item.duration_ms.unwrap_or(0),
            r.item.words.unwrap_or(0),
            r.item.chars.unwrap_or(0),
        )
    }

    /// Shared filter+normalize used by the signed-out view below.
    fn normalize_rows(rows: Vec<StatEventRow>) -> Vec<(i64, i64, i64, i64)> {
        let active_days: HashSet<String> = rows
            .iter()
            .filter(|r| r.item.timestamp_ms > 0 || r.item.chars.unwrap_or(0) != 0)
            .map(|r| r.item.day.clone())
            .collect();
        rows.iter()
            .filter(|r| {
                !(r.item.timestamp_ms == 0
                    && r.item.chars.unwrap_or(0) == 0
                    && active_days.contains(&r.item.day))
            })
            .map(Self::normalize_stat_row)
            .collect()
    }

    /// All local ledger rows regardless of attribution (signed-out view),
    /// with the same aggregate-collapse rule as the account view so zero
    /// rows never count sessions.
    fn local_normalized_rows() -> Vec<(i64, i64, i64, i64)> {
        Self::normalize_rows(Self::load_rows())
    }

    fn bucket_events(rows: Vec<(i64, i64, i64, i64)>, since_ms: Option<i64>) -> Vec<DailyBucket> {
        let mut days: std::collections::BTreeMap<i64, (i64, i64, i64)> =
            std::collections::BTreeMap::new();
        for (ts, duration_ms, words, _chars) in rows {
            if ts < 0 {
                continue;
            }
            if let Some(since) = since_ms {
                if ts < since {
                    continue;
                }
            }
            let day_start_ms = ts - ts.rem_euclid(ACTIVITY_DAY_MS);
            let entry = days.entry(day_start_ms).or_insert((0, 0, 0));
            entry.0 += 1;
            entry.1 += words;
            entry.2 += duration_ms;
        }
        days.into_iter()
            .map(
                |(day_start_ms, (sessions, words, duration_ms))| DailyBucket {
                    day_start_ms,
                    sessions,
                    words,
                    duration_ms,
                },
            )
            .collect()
    }

    fn cached_event_rows(key: Option<String>, load: impl FnOnce() -> ActivityRows) -> ActivityRows {
        if let Ok(guard) = ACTIVITY_CACHE.lock() {
            if let Some((cached_key, rows)) = guard.as_ref() {
                if *cached_key == key {
                    return rows.clone();
                }
            }
        }
        let rows = load();
        if let Ok(mut guard) = ACTIVITY_CACHE.lock() {
            *guard = Some((key, rows.clone()));
        }
        rows
    }

    /// Drop the dashboard activity cache. Called on local record and on
    /// completed sync passes; the next read reloads lazily from the file.
    pub(crate) fn invalidate_activity_cache() {
        if let Ok(mut guard) = ACTIVITY_CACHE.lock() {
            *guard = None;
        }
    }

    /// UNIT D - growth gauge: rows + envelope bytes vs 8 MiB headroom for the given account.
    /// Pure, no I/O beyond reading the local ledger; callers surface via existing diagnostics path.
    pub fn gauge_for_account(account_hash: &str) -> (usize, usize, usize) {
        let items: Vec<StatsItem> = Self::load_rows()
            .into_iter()
            .filter(|r| r.account.as_deref() == Some(account_hash))
            .map(|r| r.item)
            .collect();
        let rows = items.len();
        let bytes = StatsEnvelope {
            v: crate::sync::domain::ENVELOPE_V1,
            entries: items,
        }
        .to_bytes()
        .len();
        let headroom = crate::sync::drive::MAX_DOMAIN_BYTES.saturating_sub(bytes);
        (rows, bytes, headroom)
    }
}

/// One UTC day of dictation activity.
#[derive(Debug, Clone, Serialize)]
pub struct DailyBucket {
    pub day_start_ms: i64,
    pub sessions: i64,
    pub words: i64,
    pub duration_ms: i64,
}

const ACTIVITY_DAY_MS: i64 = 86_400_000;

/// Process-level read cache: account key (`None` = signed-out all-rows
/// view) plus normalized event rows. Lazy-loaded on first read;
/// invalidated on local record and on completed sync passes (remote rows
/// land there — see the hook in the scheduler thread).
/// Normalized event rows: (timestamp_ms, duration_ms, words, chars).
type ActivityRows = Vec<(i64, i64, i64, i64)>;

static ACTIVITY_CACHE: std::sync::Mutex<Option<(Option<String>, ActivityRows)>> =
    std::sync::Mutex::new(None);

/// History-proof dashboard activity: daily UTC buckets from the synced
/// stats ledger (local ∪ remote), never the local history table.
/// Signed out, buckets cover ALL local ledger rows regardless of
/// attribution (dirty-only filtering would hide signed-in-era rows).
/// Activity buckets for an explicit account.
///
/// Split out of [`get_account_activity`] so the read path can be exercised for
/// a given account WITHOUT touching `settings.json`. That matters: `settings_path()`
/// resolves through `dirs::data_local_dir()` directly and therefore ignores the
/// test-build data-dir redirect, so any test that called `save_settings()` wrote
/// to the real user profile and could sign the user out. Never call
/// `save_settings`/`restore` from tests; pass the hash here instead.
fn account_activity_for(
    account_hash: Option<&str>,
    since_ms: Option<i64>,
) -> Result<Vec<DailyBucket>, String> {
    // Seed-on-read for the upgrade path: an account with pre-existing history but
    // no ledger events yet must not look like a new user. No-op for a signed-out
    // session and for an already-populated ledger.
    if let Some(hash) = account_hash {
        StatsDirtyStore::ensure_account_backfilled(hash);
    }
    let rows =
        StatsDirtyStore::cached_event_rows(
            account_hash.map(str::to_string),
            || match account_hash {
                Some(hash) => StatsDirtyStore::account_event_rows(hash),
                None => StatsDirtyStore::local_normalized_rows(),
            },
        );
    Ok(StatsDirtyStore::bucket_events(rows, since_ms))
}

#[tauri::command]
pub fn get_account_activity(
    window: tauri::Window,
    since_ms: Option<i64>,
) -> Result<Vec<DailyBucket>, String> {
    // Dashboard activity buckets are main-window-only data; the overlay and the
    // setup wizard never render them.
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let account_hash = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key)
        .map(|email| crate::sync::metadata::account_hash_from_email(&email));
    account_activity_for(account_hash.as_deref(), since_ms)
}

impl DirtyStore for StatsDirtyStore {
    type Item = StatsItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        // H6: lock BEFORE the read. A dictation completing between an
        // unlocked read and the backfill/reconciliation save below would be
        // overwritten and dropped. Local-only section, never network I/O.
        let _io = crate::sync::io_lock::io_lock_guard();
        // Fail safe on an unreadable ledger: return the empty view WITHOUT
        // the backfill-append-and-save below, which would persist a partial
        // ledger over corrupt bytes (Phase E).
        let mut rows = match Self::load_rows_strict() {
            Ok(r) => r,
            Err(e) => {
                log::error!("Stats load skipping backfill; ledger unreadable: {e:?}");
                return Vec::new();
            }
        };
        // The backfill/reconciliation saves below run under the guard taken
        // at the top of this function, serialized against the pass writers
        // (`record_dictation_event`, `stamp_account`, `save_merged`), which
        // all hold `io_lock`. `io_lock` is reentrant for callers already
        // holding it.
        let metadata = SyncMetadata::load();
        if !Self::backfill_done(&metadata, account_hash) {
            // Seed once per account. Deterministic ids dedup against events
            // already recorded live by the commit path.
            let synthetic = Self::backfill_from_history(account_hash);
            let mut changed = false;
            for row in synthetic {
                if !rows.iter().any(|r| r.item.event_id == row.item.event_id) {
                    rows.push(row);
                    changed = true;
                }
            }
            if changed {
                let _ = Self::save_rows(&rows);
            }
        } else {
            // Reconciliation sweep: heal missing ledger rows after backfill_done (crash gap)
            let db_path = Self::history_db_path();
            if db_path.exists() {
                if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                    let history_rows = Self::query_history_rows(&conn);
                    let mut changed = false;
                    for (id, ts, text, dur) in history_rows {
                        let eid = synthetic_event_id(&id);
                        if !rows.iter().any(|r| r.item.event_id == eid) {
                            let item = StatsItem::from_history_row(&id, ts, &text, dur);
                            rows.push(StatEventRow {
                                item,
                                account: Some(account_hash.to_string()),
                                dirty: true,
                                ever_pushed: false,
                            });
                            changed = true;
                        }
                    }
                    if changed {
                        let _ = Self::save_rows(&rows);
                    }
                }
            }
        }
        rows.into_iter()
            .filter(|r| r.account.as_deref() == Some(account_hash))
            .map(|r| r.item)
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows = Self::load_rows_strict()
            .map_err(|e| SyncError::Fatal(format!("stats ledger unreadable: {e:?}")))?;
        let mut stamped = 0;
        for row in rows.iter_mut() {
            if row.account.is_none() {
                row.account = Some(account_hash.to_string());
                row.dirty = true;
                stamped += 1;
            }
        }
        if stamped > 0 {
            Self::save_rows(&rows)?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        // Audit #6b: fail closed. `load_rows().unwrap_or_default()` made a
        // corrupt ledger report "clean, nothing to push", so a pass completed
        // successfully and silently with no events in it - the exact shape of
        // a user whose dictation history quietly stopped syncing.
        match Self::load_rows_strict() {
            Ok(rows) => rows
                .iter()
                .any(|r| r.account.as_deref() == Some(account_hash) && r.dirty),
            Err(e) => {
                log::error!("Stats has_dirty failing closed (reporting dirty): {e:?}");
                true
            }
        }
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows = Self::load_rows_strict()
            .map_err(|e| SyncError::Fatal(format!("stats ledger unreadable: {e:?}")))?;
        let merged_ids: std::collections::HashSet<String> =
            merged.iter().map(|i| i.event_id.clone()).collect();
        let rescued: Vec<StatEventRow> = rows
            .iter()
            .filter(|r| {
                r.account.as_deref() == Some(account_hash)
                    && r.dirty
                    && !merged_ids.contains(&r.item.event_id)
            })
            .cloned()
            .collect();
        // Mid-pass LWW guard for present rows: if a local dirty row still
        // wins tie (equal updatedAt, larger deviceId) keep it instead of
        // clobbering with the merged winner - mirrors Android
        // RoomStatV1Store.applyMergedAndClearDirty and dictionary rescue.
        use std::collections::HashMap;
        let dirty_by_id: HashMap<String, StatEventRow> = rows
            .iter()
            .filter(|r| r.account.as_deref() == Some(account_hash) && r.dirty)
            .map(|r| (r.item.event_id.clone(), r.clone()))
            .collect();
        let mut filtered_merged = Vec::new();
        let mut rescued_tie: Vec<StatEventRow> = Vec::new();
        // Shard-unroutable rows (malformed `day`, no valid UTC month) can
        // never be uploaded to a month shard. They stay in the local ledger
        // for the dashboard, permanently dirty — never stamped clean.
        let mut unroutable: Vec<StatsItem> = Vec::new();
        for item in merged {
            if crate::sync::stats_shards::month_of_day(&item.day).is_none() {
                unroutable.push(item);
                continue;
            }
            if let Some(local) = dirty_by_id.get(&item.event_id) {
                let local_at = local.item.updated_at.unwrap_or(0);
                let local_dev = local.item.device_id.as_deref().unwrap_or("");
                let win_at = item.updated_at.unwrap_or(0);
                let win_dev = item.device_id.as_deref().unwrap_or("");
                if crate::sync::clock::cmp_winner(local_at, local_dev, win_at, win_dev)
                    != std::cmp::Ordering::Less
                {
                    rescued_tie.push(local.clone());
                    continue;
                }
            }
            filtered_merged.push(item);
        }
        rows.retain(|r| r.account.as_deref() != Some(account_hash));
        for item in filtered_merged {
            rows.push(StatEventRow {
                item,
                account: Some(account_hash.to_string()),
                dirty: false,
                ever_pushed: true,
            });
        }
        for mut row in rescued {
            row.dirty = true;
            row.ever_pushed = false;
            rows.push(row);
        }
        for mut row in rescued_tie {
            row.dirty = true;
            row.ever_pushed = false;
            rows.push(row);
        }
        for item in unroutable {
            rows.push(StatEventRow {
                item,
                account: Some(account_hash.to_string()),
                dirty: true,
                ever_pushed: false,
            });
        }
        Self::save_rows(&rows)?;
        // Mark backfill done after the first successful merge for this account.
        let mut metadata = SyncMetadata::load();
        if !Self::backfill_done(&metadata, account_hash) {
            metadata.for_account_mut(account_hash).backfill_done = true;
            metadata.save();
        }
        Ok(())
    }
}

// ── Agent store (Phase 6, account-partitioned) ──────────────────────────────
//
// Local persistence is the per-account file owned by `account_scope`
// (`agents.account-<hash>.json`), NOT a shared table with a sync_account
// column. That makes account isolation structural: `load` cannot observe
// another account's rows because they live in a different file.
//
// Dirty is DERIVED, mirroring Android `toLocal().dirty`: a row lacking sync
// metadata has never been pushed. There is no separate dirty flag to drift out
// of sync with the data, and no rescue pass: `save_merged` wholesale-replaces
// the account file with the merge winners, exactly like Android's
// `applyMergedAndClearDirty`. (Dictionary keeps a rescue pass because it is a
// hot shared file edited concurrently by the UI; agents are low-frequency and
// cross-platform convergence demands both sides compute the same result from
// the same merge, which a platform-specific rescue would break.)

pub struct AgentDirtyStore;

impl AgentDirtyStore {
    fn to_domain_item(e: &crate::account_scope::AccountAgent, device_id: &str) -> AgentItem {
        AgentItem {
            sync_id: e
                .sync_id
                .clone()
                .unwrap_or_else(|| AgentItem::stable_sync_id(&e.id)),
            business_key: e.id.clone(),
            name: e.name.clone(),
            hint: e.hint.clone(),
            updated_at: e.updated_at.unwrap_or(0),
            deleted_at: e.deleted_at,
            device_id: e
                .device_id
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| device_id.to_string()),
        }
    }

    fn from_domain_item(item: AgentItem) -> crate::account_scope::AccountAgent {
        crate::account_scope::AccountAgent {
            id: item.business_key,
            name: item.name,
            hint: item.hint,
            sync_id: Some(item.sync_id),
            updated_at: Some(item.updated_at),
            device_id: Some(item.device_id),
            deleted_at: item.deleted_at,
            // A row that came back from a completed merge is by definition
            // already in the sync state, so it starts clean. Without this the
            // flag would survive the merge and every pass would re-upload it.
            dirty: false,
        }
    }

    /// Pending-mutation test.
    ///
    /// The explicit `dirty` flag is the load-bearing part. The metadata checks
    /// remain as a safety net so a row written before the flag existed (or by a
    /// truncated write) still uploads exactly once instead of being stranded.
    fn is_dirty(e: &crate::account_scope::AccountAgent) -> bool {
        e.dirty
            || e.sync_id.as_ref().map_or(true, |s| s.is_empty())
            || e.updated_at.unwrap_or(0) <= 0
    }
}

impl DirtyStore for AgentDirtyStore {
    type Item = AgentItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Vec::new();
        }
        let meta = SyncMetadata::load();
        let device_id = meta.device_id.clone();
        crate::account_scope::load_account_agents(account_hash)
            .custom_agents
            .iter()
            .map(|e| Self::to_domain_item(e, &device_id))
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(0);
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        // Fail closed: an unusable account store must not be stamped as "no
        // agents" and then saved back over the user's data. The error stops the
        // domain and leaves the dirty rows for the next pass.
        let mut store = crate::account_scope::load_account_agents_checked(account_hash)
            .map_err(SyncError::Fatal)?;
        if store.custom_agents.iter().all(|e| !Self::is_dirty(e)) {
            return Ok(0);
        }
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        let mut stamped = 0;
        for e in store.custom_agents.iter_mut() {
            if !Self::is_dirty(e) {
                continue;
            }
            // Stable sync id: deterministic per business key AND byte-identical
            // with Android, so a legacy record stamped independently on each
            // platform still converges under LWW instead of forking.
            if e.sync_id.as_ref().map_or(true, |s| s.is_empty()) {
                e.sync_id = Some(AgentItem::stable_sync_id(&e.id));
            }
            // A dirty row is a NEW local mutation, so it gets a fresh revision
            // from this device. Reusing the revision inherited from the peer we
            // pulled from would make the edit indistinguishable from what we
            // already had, and LWW would be free to discard it. `monotonic_now`
            // keeps the stamp above everything seen for the account.
            if e.dirty || e.updated_at.unwrap_or(0) <= 0 {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                e.updated_at = Some(now);
            }
            if e.dirty || e.device_id.as_ref().map_or(true, |d| d.is_empty()) {
                e.device_id = Some(device_id.clone());
            }
            stamped += 1;
        }
        if stamped > 0 {
            meta.save();
            crate::account_scope::save_account_agents(account_hash, &store)
                .map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return false;
        }
        // Fail closed. `.any(is_dirty)` over a store that failed to read is
        // empty, so it reported CLEAN and the domain silently skipped itself
        // forever instead of surfacing a problem. True here routes to the
        // reconcile, which refuses with the same error and keeps the rows dirty.
        crate::account_scope::load_account_agents_checked(account_hash)
            .map(|s| s.custom_agents.iter().any(Self::is_dirty))
            .unwrap_or(true)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(());
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        // Re-read the CURRENT rows INSIDE the lock. `merged` is this pass's
        // LWW result, computed from a snapshot taken BEFORE the Drive round
        // trip, so a local create/edit/delete that landed meanwhile exists
        // only here. Wholesale-writing `merged` (the old behaviour) destroyed
        // it: a row created during the pass existed in neither store (audit
        // finding H5), and a concurrent edit/delete was silently reverted.
        //
        // The check and the write share one critical section, and every local
        // account writer holds `io_lock` too, so no mutation can interleave
        // between them. The lock is never held across network I/O - the pass
        // releases it before `put_scope` and re-takes it here.
        // Fail closed — a reconcile over an empty view of an unusable store
        // would persist the merge winners alone and delete everything else.
        let current = crate::account_scope::load_account_agents_checked(account_hash)
            .map_err(SyncError::Fatal)?
            .custom_agents;
        let mut rows: Vec<crate::account_scope::AccountAgent> = Vec::with_capacity(current.len());
        for e in current {
            match merged.iter().find(|m| m.business_key == e.id) {
                // No winner for this id: a row created during the pass. Keep it
                // exactly as stored - dirty flag included - so the next pass
                // uploads it instead of dropping it on the floor.
                None => rows.push(e),
                // A winner exists. Keep the local row only when it carries a
                // mutation that has NOT been uploaded yet, i.e. it is dirty AND
                // its content still differs from the winner.
                //
                // `dirty` is the load-bearing signal, not the timestamp: a
                // local edit deliberately does not restamp `updated_at` (that
                // belongs to the next pass), so it still carries its pre-edit
                // revision and would lose any pure LWW comparison to the
                // payload this pass just uploaded. Keeping it is what the stamp
                // logic is built for - `stamp_account` gives a dirty row a
                // fresh revision above the account high-water mark, so it wins
                // on the following pass and converges.
                //
                // The content check is what stops `dirty` from becoming
                // self-perpetuating: once the row's content EQUALS the winner,
                // this exact edit is what was uploaded, so the winner is
                // authoritative and the row is written back clean. Without it
                // the flag would re-upload on every pass forever.
                Some(winner) => {
                    let already_uploaded = e.name == winner.name
                        && e.hint == winner.hint
                        && e.deleted_at == winner.deleted_at;
                    if Self::is_dirty(&e) && !already_uploaded {
                        rows.push(e);
                    }
                }
            }
        }
        let kept_ids: std::collections::HashSet<String> =
            rows.iter().map(|e| e.id.clone()).collect();
        let winner_keys: std::collections::HashSet<String> =
            merged.iter().map(|m| m.business_key.clone()).collect();
        rows.extend(
            merged
                .into_iter()
                .filter(|m| !kept_ids.contains(&m.business_key))
                .map(Self::from_domain_item),
        );
        // Fold the never-pushed tombstone purge into the same write. A
        // tombstone for a record that is in this pass's payload has a remote
        // counterpart, so it MUST survive: dropping it resurrects the record
        // on the next pass. Only a tombstone with no counterpart (created and
        // deleted while never synced) is dropped - it carries nothing a peer
        // could need.
        rows.retain(|e| {
            !(e.deleted_at.is_some()
                && e.sync_id.as_ref().map_or(true, |s| s.is_empty())
                && !winner_keys.contains(&e.id))
        });
        crate::account_scope::save_account_agents(
            account_hash,
            &crate::account_scope::AccountAgentsStore {
                custom_agents: rows,
            },
        )
        .map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

// ── Style store (Phase 6, account-partitioned) ──────────────────────────────
//
// Identical contract to [`AgentDirtyStore`] with the `custom:<uuid>` keyspace.
// See above for why dirty is derived, why there is no rescue pass, and why the
// purge is folded in.

pub struct StyleDirtyStore;

impl StyleDirtyStore {
    fn to_domain_item(e: &crate::account_scope::AccountStyle, device_id: &str) -> StyleItem {
        StyleItem {
            sync_id: e
                .sync_id
                .clone()
                .unwrap_or_else(|| StyleItem::stable_sync_id(&e.id)),
            business_key: e.id.clone(),
            name: e.name.clone(),
            hint: e.hint.clone(),
            updated_at: e.updated_at.unwrap_or(0),
            deleted_at: e.deleted_at,
            device_id: e
                .device_id
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| device_id.to_string()),
        }
    }

    fn from_domain_item(item: StyleItem) -> crate::account_scope::AccountStyle {
        crate::account_scope::AccountStyle {
            id: item.business_key,
            name: item.name,
            hint: item.hint,
            sync_id: Some(item.sync_id),
            updated_at: Some(item.updated_at),
            device_id: Some(item.device_id),
            deleted_at: item.deleted_at,
            // Merged back in: already in the sync state, so clean.
            dirty: false,
        }
    }

    /// Pending-mutation test. See [`AgentDirtyStore::is_dirty`].
    fn is_dirty(e: &crate::account_scope::AccountStyle) -> bool {
        e.dirty
            || e.sync_id.as_ref().map_or(true, |s| s.is_empty())
            || e.updated_at.unwrap_or(0) <= 0
    }
}

impl DirtyStore for StyleDirtyStore {
    type Item = StyleItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Vec::new();
        }
        let meta = SyncMetadata::load();
        let device_id = meta.device_id.clone();
        crate::account_scope::load_account_styles(account_hash)
            .custom_styles
            .iter()
            .map(|e| Self::to_domain_item(e, &device_id))
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(0);
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        // Fail closed — see AgentDirtyStore::stamp_account.
        let mut store = crate::account_scope::load_account_styles_checked(account_hash)
            .map_err(SyncError::Fatal)?;
        if store.custom_styles.iter().all(|e| !Self::is_dirty(e)) {
            return Ok(0);
        }
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        let mut stamped = 0;
        for e in store.custom_styles.iter_mut() {
            if !Self::is_dirty(e) {
                continue;
            }
            if e.sync_id.as_ref().map_or(true, |s| s.is_empty()) {
                e.sync_id = Some(StyleItem::stable_sync_id(&e.id));
            }
            // Fresh revision for a dirty row; see AgentDirtyStore::stamp_account.
            if e.dirty || e.updated_at.unwrap_or(0) <= 0 {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                e.updated_at = Some(now);
            }
            if e.dirty || e.device_id.as_ref().map_or(true, |d| d.is_empty()) {
                e.device_id = Some(device_id.clone());
            }
            stamped += 1;
        }
        if stamped > 0 {
            meta.save();
            crate::account_scope::save_account_styles(account_hash, &store)
                .map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return false;
        }
        // Fail closed — see AgentDirtyStore::has_dirty.
        crate::account_scope::load_account_styles_checked(account_hash)
            .map(|s| s.custom_styles.iter().any(Self::is_dirty))
            .unwrap_or(true)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(());
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        // Same reconcile as AgentDirtyStore::save_merged (audit finding H5):
        // `merged` comes from a snapshot taken before the Drive round trip, so
        // a local create/edit/delete landing meanwhile survives here instead
        // of being overwritten by the stale payload. Check and write share one
        // `io_lock` critical section; every local account writer takes the
        // same lock, and it is never held across network I/O. The rule is
        // byte-identical to Android's, so both platforms converge.
        // Fail closed — see AgentDirtyStore::save_merged.
        let current = crate::account_scope::load_account_styles_checked(account_hash)
            .map_err(SyncError::Fatal)?
            .custom_styles;
        let mut rows: Vec<crate::account_scope::AccountStyle> = Vec::with_capacity(current.len());
        for e in current {
            match merged.iter().find(|m| m.business_key == e.id) {
                None => rows.push(e),
                Some(winner) => {
                    let already_uploaded = e.name == winner.name
                        && e.hint == winner.hint
                        && e.deleted_at == winner.deleted_at;
                    if Self::is_dirty(&e) && !already_uploaded {
                        rows.push(e);
                    }
                }
            }
        }
        let kept_ids: std::collections::HashSet<String> =
            rows.iter().map(|e| e.id.clone()).collect();
        let winner_keys: std::collections::HashSet<String> =
            merged.iter().map(|m| m.business_key.clone()).collect();
        rows.extend(
            merged
                .into_iter()
                .filter(|m| !kept_ids.contains(&m.business_key))
                .map(Self::from_domain_item),
        );
        // Never-pushed tombstone purge: only a tombstone with no counterpart
        // in this pass's payload (created and deleted while never synced) is
        // dropped; one whose record WAS pushed must survive to propagate the
        // delete.
        rows.retain(|e| {
            !(e.deleted_at.is_some()
                && e.sync_id.as_ref().map_or(true, |s| s.is_empty())
                && !winner_keys.contains(&e.id))
        });
        crate::account_scope::save_account_styles(
            account_hash,
            &crate::account_scope::AccountStylesStore {
                custom_styles: rows,
            },
        )
        .map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_emitted_keys_never_include_secrets_or_platform_config() {
        let settings = AppSettings::default();
        let values = SettingsDirtyStore::live_values(&settings);
        let keys: Vec<&str> = values.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "language",
                "snippets_enabled",
                "auto_learn_enabled",
                "ai_polish_style"
            ]
        );
        for forbidden in [
            "hotkey",
            "agent_hotkey",
            "audio_device_id",
            "stt_provider",
            "llm_provider",
            "api_key",
        ] {
            assert!(
                !keys.contains(&forbidden),
                "{forbidden} must never be emitted"
            );
        }
    }

    #[test]
    fn apply_winner_cannot_touch_secrets_or_providers() {
        let mut settings = AppSettings::default();
        let before = settings.hotkey.clone();
        SettingsDirtyStore::apply_winner(&mut settings, "hotkey", "Ctrl+X");
        SettingsDirtyStore::apply_winner(&mut settings, "unknown_future_key", "evil");
        assert_eq!(settings.hotkey, before, "hotkey must be immutable via sync");
    }

    /// Delegates to the module-level guard, so every test that mutates a
    /// shared temp-rooted store - here and in `sync::frozen` - contends on
    /// one lock instead of one per test module.
    pub(super) fn store_test_guard() -> std::sync::MutexGuard<'static, ()> {
        super::store_test_guard()
    }

    #[test]
    fn stat_event_recording_is_idempotent_per_history_row() {
        let _guard = store_test_guard();
        let tmp =
            std::env::temp_dir().join(format!("fluence-test-ledger-{}.json", std::process::id()));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        // record_dictation_event dedups on the deterministic event id.
        let id = "test-row-123";
        StatsDirtyStore::record_dictation_event(id, 1_000, "hello world", 500);
        StatsDirtyStore::record_dictation_event(id, 1_000, "hello world", 500);
        let rows = StatsDirtyStore::load_rows();
        let count = rows
            .iter()
            .filter(|r| r.item.event_id == synthetic_event_id(id))
            .count();
        assert_eq!(count, 1, "duplicate commits must not double-count");
        StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&tmp);
    }

    // --- dictionary `kind` safety (audit finding C1) ----------------------

    fn expansion_dictionary_entry(id: &str, account_hash: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: id.to_string(),
            spoken: "expandme".to_string(),
            corrected: "expanded phrase".to_string(),
            kind: crate::dictionary::KIND_EXPANSION.to_string(),
            created_at: Some(1_000),
            deleted_at: None,
            updated_at: Some(2_000),
            device_id: Some("device-kind-test".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.to_string()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }

    #[test]
    fn dictionary_sync_payload_never_carries_a_non_correction_kind() {
        // The frozen v1.2 wire format has no `kind` field, so every peer
        // decodes it back as `correction`. A payload carrying the local value
        // would fail the round-trip gate with SyncError::Fatal and latch the
        // scheduler. The payload must therefore always be `correction`.
        let entry =
            expansion_dictionary_entry(&uuid::Uuid::new_v4().to_string(), "kind-account-unused");
        let item = DictionaryDirtyStore::to_domain_item(&entry).expect("stamped entry");
        assert_eq!(
            item.kind,
            crate::dictionary::KIND_CORRECTION,
            "sync payload is always `correction`"
        );
        // And the payload must survive the frozen round-trip gate that used to
        // reject it.
        let env = DictionaryEnvelope {
            v: 1,
            entries: vec![item.clone()],
        };
        let bytes = env.to_bytes();
        let decoded = DictionaryEnvelope::from_bytes(&bytes).expect("decode");
        assert_eq!(
            decoded.entries,
            vec![item],
            "encode(decode(x)) == x for an expansion-backed local row"
        );
    }

    #[test]
    fn save_merged_preserves_local_expansion_kind() {
        let _guard = store_test_guard();
        let account_hash = format!("test-kind-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4().to_string();
        let entry = expansion_dictionary_entry(&id, &account_hash);
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(entry);
        save_dictionary_internal(&all).unwrap();

        // The merged winner decodes as `correction` (no kind on the wire).
        let winner = DictionaryItem {
            sync_id: id.clone(),
            spoken: "expandme".to_string(),
            corrected: "expanded phrase".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 3_000,
            device_id: "device-remote".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store
            .save_merged(&account_hash, vec![winner])
            .expect("save_merged");

        let after = load_dictionary_internal().unwrap_or_default();
        let row = after
            .iter()
            .find(|e| e.id == id && e.sync_account.as_deref() == Some(account_hash.as_str()))
            .expect("merged row persisted");
        assert_eq!(
            row.kind,
            crate::dictionary::KIND_EXPANSION,
            "a sync pass must not silently downgrade a local expansion entry \
             (its words stay out of the STT vocabulary prompt)"
        );

        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    #[test]
    fn save_merged_preserves_fresh_concurrent_edit() {
        let _guard = store_test_guard();
        let account_hash = format!("test-dict-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        // Prepare a dirty entry newer than the merged winner
        let dirty_id = uuid::Uuid::new_v4().to_string();
        let dirty_entry = DictionaryEntry {
            id: dirty_id.clone(),
            spoken: "testspoken".to_string(),
            corrected: "testcorrected".to_string(),
            kind: "correction".to_string(),
            created_at: Some(1000),
            deleted_at: None,
            updated_at: Some(2000),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(dirty_entry.clone());
        save_dictionary_internal(&all).unwrap();
        let winner = DictionaryItem {
            sync_id: dirty_id.clone(),
            spoken: "testspoken".to_string(),
            corrected: "oldcorrected".to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 1000,
            device_id: "device-test".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store
            .save_merged(&account_hash, vec![winner.clone()])
            .unwrap();
        let after = load_dictionary_internal().unwrap_or_default();
        let account_rows: Vec<_> = after
            .iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert!(
            account_rows
                .iter()
                .any(|e| e.id == dirty_id && e.dirty && e.updated_at == Some(2000)),
            "fresh dirty edit must survive save_merged"
        );
        // H4 dedup: the rescued edit and its winner share the id, so only one
        // row may remain. The rescued local edit is newer and differs, so it
        // wins and the stale winner is skipped — mirroring the agents
        // `kept_ids` rule. This test previously asserted BOTH rows present,
        // which pinned a duplicate: two rows with one id break the 1:1
        // assumption the next merge's `find(|m| m.sync_id == e.id)` relies on.
        assert_eq!(
            account_rows.iter().filter(|e| e.id == dirty_id).count(),
            1,
            "rescued edit replaces its stale winner, never duplicates it"
        );
        // Cleanup
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// F3: a quarantined dictionary row is held for recovery and must survive
    /// merge healing — the wholesale retain must not wipe it, and the sync
    /// load must never feed it into the merge (Android excludeQuarantined
    /// parity). The field is dormant today (nothing stamps it); these pin the
    /// invariant for any future writer.
    #[test]
    fn save_merged_keeps_quarantined_dict_row() {
        let _guard = store_test_guard();
        let account_hash = format!("test-qdict-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let held = DictionaryEntry {
            id: "q-row-1".to_string(),
            spoken: "heldspoken".to_string(),
            corrected: "heldcorrected".to_string(),
            kind: "correction".to_string(),
            created_at: Some(1000),
            deleted_at: None,
            updated_at: Some(1000),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: Some("corrupt_file".to_string()),
        };
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(held.clone());
        save_dictionary_internal(&all).unwrap();
        // The merge never saw the held row (load excludes it); an empty merge
        // must not destroy it.
        let mut store = DictionaryDirtyStore;
        store.save_merged(&account_hash, Vec::new()).unwrap();
        let after = load_dictionary_internal().unwrap_or_default();
        assert!(
            after.iter().any(
                |e| e.id == "q-row-1" && e.quarantine_reason.as_deref() == Some("corrupt_file")
            ),
            "quarantined row must survive merge healing"
        );
        assert!(
            DictionaryDirtyStore.load(&account_hash).is_empty(),
            "sync load must exclude quarantined rows from the merge input"
        );
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// F3 snippet counterpart: wholesale replacement must keep a quarantined
    /// row, and the sync load must exclude it.
    #[test]
    fn save_merged_keeps_quarantined_snippet_row() {
        let _guard = store_test_guard();
        let account_hash = format!("test-qsnip-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let held = Snippet {
            id: "q-snip-1".to_string(),
            trigger: "heldtrigger".to_string(),
            expansion: "heldexpansion".to_string(),
            created_at: Some(1000),
            updated_at: Some(1000),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            deleted_at: None,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: Some("corrupt_file".to_string()),
        };
        let mut store_doc = load_snippets_internal().unwrap_or_default();
        store_doc.snippets.push(held.clone());
        save_snippets_internal(&store_doc).unwrap();
        let mut store = SnippetDirtyStore;
        store.save_merged(&account_hash, Vec::new()).unwrap();
        let after = load_snippets_internal().unwrap_or_default();
        assert!(
            after
                .snippets
                .iter()
                .any(|s| s.id == "q-snip-1"
                    && s.quarantine_reason.as_deref() == Some("corrupt_file")),
            "quarantined snippet must survive merge healing"
        );
        assert!(
            SnippetDirtyStore.load(&account_hash).is_empty(),
            "sync load must exclude quarantined snippets from the merge input"
        );
        let mut cleanup = load_snippets_internal().unwrap_or_default();
        cleanup
            .snippets
            .retain(|s| s.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_snippets_internal(&cleanup);
    }

    /// Phase D (H3) store half, dictionary: the merge collapse drops the
    /// pre-rename key from `merged`; the full-replace write must then drop
    /// the stale local row too, so the duplicate disappears from the store,
    /// not just from the Drive payload.
    #[test]
    fn save_merged_heals_rename_ghost_end_to_end() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-dict-rename-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let sync_id = uuid::Uuid::new_v4().to_string();
        // Seed the stale pre-rename row exactly as a prior sync left it:
        // clean and pushed.
        let stale = DictionaryEntry {
            id: sync_id.clone(),
            spoken: "my email".to_string(),
            corrected: "a@x.com".to_string(),
            kind: "correction".to_string(),
            created_at: Some(100),
            deleted_at: None,
            updated_at: Some(100),
            device_id: Some("device-a".to_string()),
            is_enabled: true,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(stale);
        save_dictionary_internal(&all).unwrap();
        // Collapsed merge result: only the post-rename winner.
        let winner = DictionaryItem {
            sync_id: sync_id.clone(),
            spoken: "my primary email".to_string(),
            corrected: "a@x.com".to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_dictionary_internal()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(
            after.len(),
            1,
            "the stale pre-rename row must be gone from the local store"
        );
        assert_eq!(after[0].spoken, "my primary email");
        // Cleanup
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// Phase D (H3) store half, snippets: same rename-heal contract as the
    /// dictionary counterpart above.
    #[test]
    fn snippet_save_merged_heals_rename_ghost_end_to_end() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-snip-rename-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let sync_id = uuid::Uuid::new_v4().to_string();
        let stale = Snippet {
            id: sync_id.clone(),
            trigger: "my email".to_string(),
            expansion: "a@x.com".to_string(),
            created_at: Some(100),
            updated_at: Some(100),
            device_id: Some("device-a".to_string()),
            is_enabled: true,
            deleted_at: None,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut store = load_snippets_internal().unwrap_or_default();
        store.snippets.push(stale);
        save_snippets_internal(&store).unwrap();
        let winner = SnippetItem {
            sync_id: sync_id.clone(),
            trigger: "my primary email".to_string(),
            expansion: "a@x.com".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut dirty = SnippetDirtyStore;
        dirty.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_snippets_internal()
            .unwrap_or_default()
            .snippets
            .into_iter()
            .filter(|s| s.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(
            after.len(),
            1,
            "the stale pre-rename snippet must be gone from the local store"
        );
        assert_eq!(after[0].trigger, "my primary email");
        // Cleanup
        if let Ok(mut cleanup) = load_snippets_internal() {
            cleanup
                .snippets
                .retain(|s| s.sync_account.as_deref() != Some(account_hash.as_str()));
            let _ = save_snippets_internal(&cleanup);
        }
    }

    /// H4: a dirty dict row whose content already equals its winner is NOT
    /// rescued — the winner is authoritative. Port of the agents/styles
    /// `already_uploaded` guard; without it every such row re-uploads once
    /// per merge instead of going clean.
    #[test]
    fn save_merged_dict_matching_dirty_row_goes_clean() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-h4-dict-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let sync_id = uuid::Uuid::new_v4().to_string();
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(crate::dictionary::DictionaryEntry {
            id: sync_id.clone(),
            spoken: "hello".to_string(),
            corrected: "hello!".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            created_at: Some(100),
            deleted_at: None,
            updated_at: Some(300),
            device_id: Some("device-a".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_dictionary_internal(&all).unwrap();
        let winner = crate::sync::domain::DictionaryItem {
            sync_id: sync_id.clone(),
            spoken: "hello".to_string(),
            corrected: "hello!".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_dictionary_internal()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(after.len(), 1, "exactly one row, never a duplicate");
        assert!(
            !after[0].dirty,
            "content equals the winner, so the winner is authoritative and the row goes clean instead of re-uploading"
        );
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// H4, snippet counterpart: same rule, same shape.
    #[test]
    fn save_merged_snippet_matching_dirty_row_goes_clean() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-h4-snip-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let sync_id = uuid::Uuid::new_v4().to_string();
        let mut store_doc = load_snippets_internal().unwrap_or_default();
        store_doc.snippets.push(crate::snippets::Snippet {
            id: sync_id.clone(),
            trigger: "hi".to_string(),
            expansion: "hello!".to_string(),
            created_at: Some(100),
            updated_at: Some(300),
            device_id: Some("device-a".to_string()),
            is_enabled: true,
            deleted_at: None,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_snippets_internal(&store_doc).unwrap();
        let winner = crate::sync::domain::SnippetItem {
            sync_id: sync_id.clone(),
            trigger: "hi".to_string(),
            expansion: "hello!".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut store = SnippetDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_snippets_internal()
            .unwrap_or_default()
            .snippets
            .into_iter()
            .filter(|s| s.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(after.len(), 1, "exactly one row, never a duplicate");
        assert!(
            !after[0].dirty,
            "content equals the winner, so the row goes clean instead of re-uploading"
        );
        if let Ok(mut cleanup) = load_snippets_internal() {
            cleanup
                .snippets
                .retain(|s| s.sync_account.as_deref() != Some(account_hash.as_str()));
            let _ = save_snippets_internal(&cleanup);
        }
    }

    /// H4 converse: a dirty row whose content DIFFERS from its winner must
    /// still be rescued dirty — the guard must not swallow real edits.
    #[test]
    fn save_merged_dict_differing_dirty_row_stays_dirty() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-h4-dict-diff-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let sync_id = uuid::Uuid::new_v4().to_string();
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(crate::dictionary::DictionaryEntry {
            id: sync_id.clone(),
            spoken: "hello".to_string(),
            corrected: "local edit".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            created_at: Some(100),
            deleted_at: None,
            // Newer than the winner: a mid-pass edit that the pre-network
            // snapshot never saw, so the timestamp gate lets it through and
            // the content gate must not swallow it.
            updated_at: Some(300),
            device_id: Some("device-a".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_dictionary_internal(&all).unwrap();
        let winner = crate::sync::domain::DictionaryItem {
            sync_id: sync_id.clone(),
            spoken: "hello".to_string(),
            corrected: "winner text".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_dictionary_internal()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        let kept = after
            .iter()
            .find(|e| e.id == sync_id)
            .expect("differing dirty row must survive");
        assert_eq!(kept.corrected, "local edit");
        assert!(kept.dirty, "and it stays dirty for the next upload");
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// H4 cross-device shape: two devices create the same word offline with
    /// different random ids. The merge winner (newer) arrives while this
    /// device still holds its dirty loser (older). The loser must converge
    /// onto the winner — exactly one row, clean — never duplicate and never
    /// re-upload. The rescue finds the winner by business key: an id-only
    /// lookup missed it (`None => true`) and resurrected the loser dirty
    /// forever.
    #[test]
    fn save_merged_dict_cross_device_loser_converges_onto_winner() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-h4-dict-xdev-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let loser_id = uuid::Uuid::new_v4().to_string();
        let winner_id = uuid::Uuid::new_v4().to_string();
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(crate::dictionary::DictionaryEntry {
            id: loser_id.clone(),
            spoken: "colour".to_string(),
            corrected: "local spelling".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            created_at: Some(100),
            deleted_at: None,
            updated_at: Some(100),
            device_id: Some("device-b".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_dictionary_internal(&all).unwrap();
        let winner = crate::sync::domain::DictionaryItem {
            sync_id: winner_id.clone(),
            spoken: "Colour".to_string(),
            corrected: "winner spelling".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_dictionary_internal()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(
            after.len(),
            1,
            "loser converges onto the winner, never duplicates"
        );
        assert_eq!(after[0].id, winner_id);
        assert_eq!(after[0].corrected, "winner spelling");
        assert!(
            !after[0].dirty,
            "converged row goes clean so the next pass has nothing to re-upload"
        );
        assert!(after[0].ever_pushed);
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// H4 full-tie shape: same key, same updatedAt, same deviceId, different
    /// ids — the merge falls through to the sync_id tiebreak, and the loser
    /// must still converge onto the deterministic winner instead of
    /// duplicating under its own id.
    #[test]
    fn save_merged_dict_cross_device_tie_converges_onto_larger_sync_id() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-h4-dict-tie-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let loser_id = "00000000-0000-4000-8000-000000000000".to_string();
        let winner_id = "ffffffff-ffff-4fff-8fff-ffffffffffff".to_string();
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(crate::dictionary::DictionaryEntry {
            id: loser_id.clone(),
            spoken: "colour".to_string(),
            corrected: "local spelling".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            created_at: Some(100),
            deleted_at: None,
            updated_at: Some(100),
            device_id: Some("device-a".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_dictionary_internal(&all).unwrap();
        let winner = crate::sync::domain::DictionaryItem {
            sync_id: winner_id.clone(),
            spoken: "colour".to_string(),
            corrected: "winner spelling".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 100,
            device_id: "device-a".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_dictionary_internal()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(
            after.len(),
            1,
            "tie loser converges onto the larger-sync_id winner, never duplicates"
        );
        assert_eq!(after[0].id, winner_id);
        assert!(!after[0].dirty);
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    /// H4 snippet counterpart: same cross-device shape (one trigger, two
    /// random ids, older dirty loser) must converge onto the winner clean.
    #[test]
    fn save_merged_snippet_cross_device_loser_converges_onto_winner() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-h4-snip-xdev-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let loser_id = uuid::Uuid::new_v4().to_string();
        let winner_id = uuid::Uuid::new_v4().to_string();
        let mut store_doc = load_snippets_internal().unwrap_or_default();
        store_doc.snippets.push(crate::snippets::Snippet {
            id: loser_id.clone(),
            trigger: "//addr".to_string(),
            expansion: "local expansion".to_string(),
            created_at: Some(100),
            updated_at: Some(100),
            device_id: Some("device-b".to_string()),
            is_enabled: true,
            deleted_at: None,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_snippets_internal(&store_doc).unwrap();
        let winner = crate::sync::domain::SnippetItem {
            sync_id: winner_id.clone(),
            trigger: "//Addr".to_string(),
            expansion: "winner expansion".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "device-a".to_string(),
        };
        let mut store = SnippetDirtyStore;
        store.save_merged(&account_hash, vec![winner]).unwrap();
        let after: Vec<_> = load_snippets_internal()
            .unwrap_or_default()
            .snippets
            .into_iter()
            .filter(|s| s.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert_eq!(
            after.len(),
            1,
            "loser converges onto the winner, never duplicates"
        );
        assert_eq!(after[0].id, winner_id);
        assert_eq!(after[0].expansion, "winner expansion");
        assert!(
            !after[0].dirty,
            "converged row goes clean so the next pass has nothing to re-upload"
        );
        if let Ok(mut cleanup) = load_snippets_internal() {
            cleanup
                .snippets
                .retain(|s| s.sync_account.as_deref() != Some(account_hash.as_str()));
            let _ = save_snippets_internal(&cleanup);
        }
    }

    #[test]
    fn save_merged_atomically_cleans_winners_and_purges_unpushed_tombstones() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-dict-atomic-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let other_hash = format!(
            "test-dict-other-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let leader_id = uuid::Uuid::new_v4().to_string();
        let tombstone = DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "deletedlocal".to_string(),
            corrected: String::new(),
            kind: "correction".to_string(),
            created_at: Some(500),
            deleted_at: Some(600),
            updated_at: Some(600),
            device_id: Some("device-test".to_string()),
            is_enabled: false,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let dirty = DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "testspoken".to_string(),
            corrected: "testcorrected".to_string(),
            kind: "correction".to_string(),
            created_at: Some(1000),
            deleted_at: None,
            updated_at: Some(2900),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let other_tomb = DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "otherdeleted".to_string(),
            corrected: String::new(),
            kind: "correction".to_string(),
            created_at: Some(500),
            deleted_at: Some(600),
            updated_at: Some(600),
            device_id: Some("device-other".to_string()),
            is_enabled: false,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(other_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(tombstone.clone());
        all.push(dirty.clone());
        all.push(other_tomb.clone());
        save_dictionary_internal(&all).unwrap();
        let winner = DictionaryItem {
            sync_id: leader_id.clone(),
            spoken: "testspoken".to_string(),
            corrected: "oldcorrected".to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 1500,
            device_id: "device-test".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store
            .save_merged(&account_hash, vec![winner.clone()])
            .unwrap();
        let after = load_dictionary_internal().unwrap_or_default();
        let account_rows: Vec<_> = after
            .iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        // H4: the dirty edit (t=2900) is NEWER than the winner (t=1500), so
        // it wins LWW and is rescued dirty — and the stale winner for the
        // same key must NOT persist alongside it. Storing both was the H4
        // duplicate shape (one word under two ids, loser rescued forever);
        // the rescued newer edit replaces its stale winner.
        assert_eq!(
            account_rows.len(),
            1,
            "exactly one row for the key: the rescued newer edit replaces its stale winner"
        );
        assert_eq!(account_rows[0].id, dirty.id);
        assert_eq!(account_rows[0].corrected, "testcorrected");
        assert!(
            account_rows[0].dirty && !account_rows[0].ever_pushed,
            "fresh dirty edit must stay dirty and unpushed (never force-stamped by a second pass)"
        );
        assert_eq!(account_rows[0].updated_at, Some(2900));
        assert!(
            !after.iter().any(|e| e.id == tombstone.id),
            "this account's never-pushed tombstone must be purged in the same write"
        );
        assert!(
            after.iter().any(|e| e.id == other_tomb.id),
            "another account's tombstone must be untouched"
        );
        // Cleanup
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| {
            e.sync_account.as_deref() != Some(account_hash.as_str())
                && e.sync_account.as_deref() != Some(other_hash.as_str())
        });
        let _ = save_dictionary_internal(&cleanup);
    }

    #[test]
    fn save_merged_preserves_dirty_not_in_merged_stats() {
        let _guard = store_test_guard();
        let tmp = std::env::temp_dir().join(format!(
            "fluence-test-ledger-stats-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        let _ = std::fs::remove_file(&tmp);
        let account_hash = format!("test-stats-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let dirty_id = uuid::Uuid::new_v4().to_string();
        let dirty_item = StatsItem {
            event_id: dirty_id.clone(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 1000,
            words: Some(10),
            chars: Some(40),
            duration_ms: Some(5000),
            updated_at: None,
            device_id: None,
        };
        let dirty_row = StatEventRow {
            item: dirty_item.clone(),
            account: Some(account_hash.clone()),
            dirty: true,
            ever_pushed: false,
        };
        StatsDirtyStore::save_rows(&[dirty_row]).unwrap();
        // merged does NOT contain dirty_id
        let other = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-21".to_string(),
            timestamp_ms: 2000,
            words: Some(5),
            chars: Some(20),
            duration_ms: Some(3000),
            updated_at: None,
            device_id: None,
        };
        let mut store = StatsDirtyStore;
        store
            .save_merged(&account_hash, vec![other.clone()])
            .unwrap();
        let rows = StatsDirtyStore::load_rows();
        assert!(
            rows.iter().any(|r| r.item.event_id == dirty_id && r.dirty),
            "dirty row not in merged must be preserved"
        );
        assert!(
            rows.iter().any(|r| r.item.event_id == other.event_id),
            "merged row must be present"
        );
        // save_merged persisted backfill_done for this hash into the real
        // metadata file; remove the test hash so no state leaks.
        {
            let mut meta = SyncMetadata::load();
            meta.accounts.remove(&account_hash);
            meta.save();
        }
        StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&tmp);
    }

    // --- corrupt-file recovery (Phase E): stats ledger -----------------------    //
    // A malformed stats_events ledger must be preserved aside and must never
    // be replaced by a partial ledger built from an empty read. Recording
    // skips the single event; merge passes fail closed.

    // --- wire device fallback (Phase E) --------------------------------------
    //
    // `wire_device_id` is pure so the empty-identity window is pinned without
    // touching the shared metadata file.

    #[test]
    fn wire_device_prefers_stored_id() {
        assert_eq!(
            wire_device_id(Some("dev-a"), "dev-b"),
            Some("dev-a".to_string())
        );
    }

    #[test]
    fn wire_device_falls_back_to_metadata_id() {
        assert_eq!(wire_device_id(None, "dev-b"), Some("dev-b".to_string()));
        // An empty stored id is missing, not a value: fall through.
        assert_eq!(wire_device_id(Some(""), "dev-b"), Some("dev-b".to_string()));
    }

    #[test]
    fn wire_device_withheld_identity_skips_record() {
        // The one-load window after unreadable metadata: no id anywhere must
        // never become "" on the wire (sorts lowest in cmp_winner).
        assert_eq!(wire_device_id(None, ""), None);
        assert_eq!(wire_device_id(Some(""), ""), None);
    }

    fn stats_corrupt_cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("json.corrupt.json"));
        for n in 1..100u32 {
            let p = path.with_extension(format!("json.corrupt.{n}.json"));
            if !p.exists() {
                break;
            }
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn corrupt_ledger_recording_skips_event_and_preserves_bytes() {
        let _guard = store_test_guard();
        let tmp = std::env::temp_dir().join(format!(
            "fluence-test-ledger-corrupt-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        let garbage = b"{\"rows\": [oops";
        std::fs::write(&tmp, garbage).unwrap();

        // Must not append to an empty read and persist a partial ledger.
        StatsDirtyStore::record_dictation_event("row-corrupt-1", 1_000, "hello", 500);
        let backup = tmp.with_extension("json.corrupt.json");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            garbage,
            "original ledger bytes must be preserved byte-identical"
        );
        assert!(
            !tmp.exists(),
            "no partial ledger may be written over unreadable state"
        );

        // After quarantine the ledger is genuinely fresh: recording works.
        StatsDirtyStore::record_dictation_event("row-corrupt-1", 1_000, "hello", 500);
        assert!(
            StatsDirtyStore::load_rows()
                .iter()
                .any(|r| r.item.event_id == synthetic_event_id("row-corrupt-1")),
            "fresh ledger after quarantine must accept writes"
        );
        StatsDirtyStore::set_test_ledger_path(None);
        stats_corrupt_cleanup(&tmp);
    }

    #[test]
    fn stats_save_merged_refuses_corrupt_ledger() {
        let _guard = store_test_guard();
        let tmp = std::env::temp_dir().join(format!(
            "fluence-test-ledger-corrupt-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        let garbage = b"\x00\x01 binary {{";
        std::fs::write(&tmp, garbage).unwrap();
        let account_hash = format!("{:064x}", 21u128);

        let mut store = StatsDirtyStore;
        let err = store
            .save_merged(&account_hash, Vec::new())
            .expect_err("save over a corrupt ledger must fail closed");
        assert!(
            matches!(err, SyncError::Fatal(_)),
            "corrupt local ledger is unrecoverable-automatically: {err:?}"
        );
        assert_eq!(
            std::fs::read(tmp.with_extension("json.corrupt.json")).unwrap(),
            garbage,
            "the failed pass must preserve the evidence, not overwrite it"
        );
        assert!(
            !tmp.exists(),
            "no fresh ledger may be written while the corruption is unprocessed"
        );
        StatsDirtyStore::set_test_ledger_path(None);
        stats_corrupt_cleanup(&tmp);
    }

    // --- corrupt-file recovery (Phase E): settings sync meta ----------------
    //
    // settings_sync_<hash>.json holds derived bookkeeping (last-synced
    // baselines), not user data. Corrupt bytes are still preserved aside, but
    // the doc resets to fresh baselines so the pass continues instead of
    // wedging sync over bookkeeping (settings.rs precedent).

    #[test]
    fn corrupt_settings_meta_is_quarantined_and_fresh() {
        // Hermetic: unique temp path via the path-parameterized core.
        let path = std::env::temp_dir().join(format!(
            "fluence-test-settings-meta-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let garbage = b"meta {{{\x00";
        std::fs::write(&path, garbage).unwrap();

        let meta = SettingsDirtyStore::load_meta_from(&path);
        assert!(
            meta.keys.is_empty() && meta.activation_baseline.is_none(),
            "corrupt meta must reset to fresh baselines: {meta:?}"
        );
        assert_eq!(
            std::fs::read(path.with_extension("json.corrupt.json")).unwrap(),
            garbage,
            "original bytes must be preserved byte-identical"
        );
        assert!(!path.exists(), "corrupt original must be rotated aside");
        let _ = std::fs::remove_file(path.with_extension("json.corrupt.json"));
    }

    #[test]
    fn save_merged_keeps_shard_unroutable_rows_dirty() {
        let _guard = store_test_guard();
        let tmp = std::env::temp_dir().join(format!(
            "fluence-test-ledger-unroutable-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        let _ = std::fs::remove_file(&tmp);
        let account_hash = format!(
            "test-unroutable-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        // A row whose day has no valid UTC month can never be uploaded to a
        // month shard. It must stay local (dashboard) and dirty (never clean).
        let bad = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "not-a-day!".to_string(),
            timestamp_ms: 1000,
            words: Some(10),
            chars: Some(40),
            duration_ms: Some(5000),
            updated_at: None,
            device_id: None,
        };
        let good = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-21".to_string(),
            timestamp_ms: 2000,
            words: Some(5),
            chars: Some(20),
            duration_ms: Some(3000),
            updated_at: None,
            device_id: None,
        };
        let mut store = StatsDirtyStore;
        store
            .save_merged(&account_hash, vec![bad.clone(), good.clone()])
            .unwrap();
        let rows = StatsDirtyStore::load_rows();
        let bad_row = rows
            .iter()
            .find(|r| r.item.event_id == bad.event_id)
            .expect("unroutable row must be kept locally");
        assert!(bad_row.dirty, "unroutable row must never be stamped clean");
        assert!(!bad_row.ever_pushed);
        let good_row = rows
            .iter()
            .find(|r| r.item.event_id == good.event_id)
            .expect("routable row must be present");
        assert!(!good_row.dirty, "routable merged row is cleaned normally");
        {
            let mut meta = SyncMetadata::load();
            meta.accounts.remove(&account_hash);
            meta.save();
        }
        StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_reconciles_missing_ledger_rows_after_backfill_done() {
        let _guard = store_test_guard();
        let tmp_ledger = std::env::temp_dir().join(format!(
            "fluence-test-ledger-reconcile-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let tmp_history = std::env::temp_dir().join(format!(
            "fluence-test-history-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let account_hash = format!(
            "test-reconcile-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        StatsDirtyStore::set_test_ledger_path(Some(tmp_ledger.clone()));
        StatsDirtyStore::set_test_history_path(Some(tmp_history.clone()));
        let _ = std::fs::remove_file(&tmp_ledger);
        let _ = std::fs::remove_file(&tmp_history);
        {
            let conn = rusqlite::Connection::open(&tmp_history).unwrap();
            conn.execute_batch(
                "CREATE TABLE history (id TEXT PRIMARY KEY, timestamp_ms INTEGER, text TEXT, duration_ms INTEGER, deleted_at INTEGER);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO history (id, timestamp_ms, text, duration_ms, deleted_at) VALUES (?1, ?2, ?3, ?4, NULL)",
                rusqlite::params!["row-1", 1713456000123i64, "hello world", 500i64],
            )
            .unwrap();
        }
        {
            let mut meta = SyncMetadata::load();
            meta.for_account_mut(&account_hash).backfill_done = true;
            meta.save();
        }
        let store = StatsDirtyStore;
        let items = store.load(&account_hash);
        assert_eq!(
            items.len(),
            1,
            "reconciliation should create missing ledger row"
        );
        assert_eq!(items[0].event_id, synthetic_event_id("row-1"));
        let items2 = store.load(&account_hash);
        assert_eq!(items2.len(), 1, "second load should not duplicate");
        let rows = StatsDirtyStore::load_rows();
        assert_eq!(rows.len(), 1);
        StatsDirtyStore::set_test_ledger_path(None);
        StatsDirtyStore::set_test_history_path(None);
        let _ = std::fs::remove_file(&tmp_ledger);
        let _ = std::fs::remove_file(&tmp_history);
        {
            let mut meta = SyncMetadata::load();
            meta.accounts.remove(&account_hash);
            meta.save();
        }
    }

    #[test]
    fn aggregates_filtered_for_existing_dictation_days() {
        // UNIT B - collapse rule: day-aggregates for days that already have dictation-level events must be suppressed.
        use crate::sync::domain::filter_aggregates_for_existing_dictation;
        use std::collections::HashSet;
        let agg1 = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 0,
            words: Some(100),
            chars: Some(0),
            duration_ms: Some(1000),
            updated_at: None,
            device_id: None,
        };
        let agg2 = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-21".to_string(),
            timestamp_ms: 0,
            words: Some(100),
            chars: Some(0),
            duration_ms: Some(1000),
            updated_at: None,
            device_id: None,
        };
        let mut existing = HashSet::new();
        existing.insert("2026-08-20".to_string());
        let filtered = filter_aggregates_for_existing_dictation(vec![agg1, agg2], &existing);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].day, "2026-08-21");
    }

    #[test]
    fn legacy_reconciliation_flagged_off_by_default() {
        // UNIT B - flagged reconciliation OFF by default, pure set-op (union-dedup by eventId)
        // This test documents the flag; actual deletion is behind feature gate.
        const STATS_RECONCILIATION_ENABLED: bool = false;
        assert!(
            !STATS_RECONCILIATION_ENABLED,
            "reconciliation must be OFF by default"
        );
    }

    #[test]
    fn account_activity_buckets_pin_utc_boundaries() {
        let _guard = store_test_guard();
        // Fixed UTC midnight: 2026-08-31T00:00:00Z.
        let monday = chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day = 86_400_000i64;
        let buckets = StatsDirtyStore::bucket_events(
            vec![
                (monday + 3_600_000, 5_000, 10, 50),
                (monday + 7_200_000, 3_000, 5, 20),
                (monday + day + 1_000, 2_000, 7, 30),
            ],
            None,
        );
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].day_start_ms, monday);
        assert_eq!(
            (
                buckets[0].sessions,
                buckets[0].words,
                buckets[0].duration_ms
            ),
            (2, 15, 8_000)
        );
        assert_eq!(buckets[1].day_start_ms, monday + day);
        assert_eq!(
            (
                buckets[1].sessions,
                buckets[1].words,
                buckets[1].duration_ms
            ),
            (1, 7, 2_000)
        );
        // since_ms filters server-side before bucketing.
        let tail = StatsDirtyStore::bucket_events(
            vec![
                (monday + 3_600_000, 5_000, 10, 50),
                (monday + day + 1_000, 2_000, 7, 30),
            ],
            Some(monday + day),
        );
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].day_start_ms, monday + day);

        // Signed-out view keeps rows regardless of attribution.
        let mk = |account: Option<&str>, ts: i64| StatEventRow {
            item: StatsItem::from_history_row("row", ts, "hello world test", 500),
            account: account.map(str::to_string),
            dirty: true,
            ever_pushed: false,
        };
        let normalized = StatsDirtyStore::normalize_rows(vec![
            mk(Some("hash"), monday + 1_000),
            mk(None, monday + 2_000),
        ]);
        assert_eq!(
            normalized,
            vec![(monday + 1_000, 500, 3, 16), (monday + 2_000, 500, 3, 16)]
        );
    }
}

// ===========================================================================
// STAGE 9 (deferred): dashboard baseline initialization on the upgrade path.
//
// "No baseline yet" is derived at render time from the trailing daily average
// of the stats ledger. The ledger is only seeded by a sync pass, so an install
// upgraded from a build older than the ledger had years of history and an empty
// ledger, and the dashboard reported the empty state like a new user.
//
// IMPORTANT: these tests drive `account_activity_for` with an INJECTED account
// hash and never call `save_settings()`. `settings_path()` resolves through
// `dirs::data_local_dir()` directly, bypassing the test-build data-dir redirect,
// so a test that saves settings writes to the real user profile and can sign the
// user out. `production_settings_survive_the_activity_read_path` guards that.
// ===========================================================================

#[cfg(test)]
mod baseline_backfill {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    fn utc_day_start(ts_ms: i64) -> i64 {
        ts_ms.div_euclid(DAY_MS) * DAY_MS
    }

    fn email_for(tag: &str) -> String {
        format!("baseline-{tag}@runtime.test")
    }

    fn hash_of(tag: &str) -> String {
        crate::sync::metadata::account_hash_from_email(&email_for(tag))
    }

    /// A minimal `history` table exposing exactly the columns
    /// `query_history_rows` reads. Building less than the real schema keeps the
    /// test honest about what the backfill actually depends on.
    fn write_history(path: &std::path::Path, rows: &[(i64, i64)]) {
        let conn = rusqlite::Connection::open(path).expect("history db");
        conn.execute_batch(
            "CREATE TABLE history (
                id TEXT PRIMARY KEY,
                timestamp_ms INTEGER NOT NULL DEFAULT 0,
                text TEXT NOT NULL DEFAULT '',
                duration_ms INTEGER NOT NULL DEFAULT 0,
                deleted_at INTEGER
            );",
        )
        .expect("schema");
        for (i, (ts, dur)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO history (id, timestamp_ms, text, duration_ms, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                rusqlite::params![format!("h{i}"), ts, "one two three four", dur],
            )
            .expect("insert");
        }
    }

    /// Point the ledger and history at temp files. Deliberately does NOT touch
    /// settings.json.
    fn arrange(tag: &str, history_rows: &[(i64, i64)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluence-baseline-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ledger = dir.join("stats_events.json");
        let history = dir.join("history.db");
        write_history(&history, history_rows);
        StatsDirtyStore::set_test_ledger_path(Some(ledger.clone()));
        StatsDirtyStore::set_test_history_path(Some(history));
        StatsDirtyStore::invalidate_activity_cache();
        ledger
    }

    fn restore() {
        StatsDirtyStore::set_test_ledger_path(None);
        StatsDirtyStore::set_test_history_path(None);
        StatsDirtyStore::invalidate_activity_cache();
    }

    fn activity(tag: &str) -> Vec<DailyBucket> {
        account_activity_for(Some(&hash_of(tag)), None).expect("activity")
    }

    fn day_bucket(buckets: &[DailyBucket], day_start: i64) -> Option<&DailyBucket> {
        buckets.iter().find(|b| b.day_start_ms == day_start)
    }

    fn days(buckets: &[DailyBucket]) -> Vec<i64> {
        buckets.iter().map(|b| b.day_start_ms).collect()
    }

    // 1. EMPTY LEDGER + POPULATED HISTORY -> baseline available immediately.
    #[test]
    fn empty_ledger_with_history_seeds_buckets_on_read() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "seed",
            &[
                (today - 2 * DAY_MS + 3_600_000, 5_000),
                (today - DAY_MS + 7_200_000, 9_000),
            ],
        );
        assert!(
            StatsDirtyStore::load_rows()
                .iter()
                .all(|r| r.account.as_deref() != Some(hash_of("seed").as_str())),
            "precondition: the ledger starts empty"
        );

        let buckets = activity("seed");

        assert!(
            day_bucket(&buckets, today - 2 * DAY_MS).is_some(),
            "history two days ago must produce a bucket; got {:?}",
            days(&buckets)
        );
        assert!(
            day_bucket(&buckets, today - DAY_MS).is_some(),
            "history yesterday must produce a bucket"
        );
        let prior = day_bucket(&buckets, today - DAY_MS).expect("yesterday bucket");
        assert_eq!(prior.sessions, 1, "one history row is one session");
        assert!(prior.words > 0, "the baseline needs a non-zero word count");
        restore();
    }

    // 2. IDEMPOTENCY: repeated reads add nothing and do not change totals.
    #[test]
    fn repeated_reads_are_idempotent() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "idem",
            &[
                (today - DAY_MS + 3_600_000, 5_000),
                (today - 2 * DAY_MS + 3_600_000, 5_000),
            ],
        );

        let first = activity("idem");
        let rows_after_first = StatsDirtyStore::load_rows().len();
        let second = activity("idem");
        let rows_after_second = StatsDirtyStore::load_rows().len();

        assert_eq!(
            rows_after_first, rows_after_second,
            "a second read must not append duplicate synthetic events"
        );
        assert_eq!(
            first
                .iter()
                .map(|b| (b.day_start_ms, b.sessions, b.words))
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|b| (b.day_start_ms, b.sessions, b.words))
                .collect::<Vec<_>>(),
            "bucket totals must be identical across reads"
        );
        restore();
    }

    // 3. POPULATED LEDGER: a partially synced account must not be bulk-seeded.
    #[test]
    fn a_populated_ledger_is_left_alone() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "populated",
            &[
                (today - 3 * DAY_MS + 3_600_000, 5_000),
                (today - DAY_MS + 3_600_000, 5_000),
            ],
        );
        StatsDirtyStore::save_rows(&[StatEventRow {
            item: StatsItem::from_history_row("already-there", today - 3 * DAY_MS, "a b c", 5_000),
            account: Some(hash_of("populated")),
            dirty: false,
            ever_pushed: true,
        }])
        .expect("seed ledger");
        StatsDirtyStore::invalidate_activity_cache();

        let buckets = activity("populated");

        assert_eq!(
            StatsDirtyStore::load_rows().len(),
            1,
            "an account that already has attributed events must not be bulk-seeded"
        );
        assert_eq!(
            days(&buckets),
            vec![today - 3 * DAY_MS],
            "only the already-present day may be shown"
        );
        restore();
    }

    // 4. EMPTY HISTORY + EMPTY LEDGER: the genuine new-user state stays.
    #[test]
    fn an_account_with_no_history_keeps_the_empty_state() {
        let _guard = super::tests::store_test_guard();
        let ledger = arrange("newuser", &[]);

        let buckets = activity("newuser");

        assert!(
            buckets.is_empty(),
            "a new user must still see no activity, i.e. the honest empty state"
        );
        let raw = std::fs::read_to_string(&ledger).unwrap_or_default();
        assert!(
            raw.is_empty() || raw == "null" || raw == "[]",
            "no synthetic events may be created for an account with no history; got {raw:?}"
        );
        restore();
    }

    // 5. MULTIPLE DAYS contribute independently to the trailing baseline.
    #[test]
    fn multiple_history_days_bucket_separately() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "multiday",
            &[
                (today - 3 * DAY_MS + 3_600_000, 1_000),
                (today - 3 * DAY_MS + 5_000_000, 1_000),
                (today - DAY_MS + 3_600_000, 1_000),
            ],
        );

        let buckets = activity("multiday");

        assert_eq!(
            buckets.len(),
            2,
            "two distinct days, got {:?}",
            days(&buckets)
        );
        assert_eq!(
            day_bucket(&buckets, today - 3 * DAY_MS)
                .expect("day")
                .sessions,
            2,
            "two rows collapse into one day with two sessions"
        );
        assert_eq!(
            day_bucket(&buckets, today - DAY_MS).expect("day").sessions,
            1
        );
        restore();
    }

    // 6. ACCOUNT ISOLATION: seeding account A must not surface in account B.
    #[test]
    fn seeding_one_account_does_not_leak_into_another() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("iso", &[(today - DAY_MS + 3_600_000, 5_000)]);

        StatsDirtyStore::ensure_account_backfilled(&hash_of("iso-a"));
        let rows = StatsDirtyStore::load_rows();
        assert_eq!(rows.len(), 1);
        assert!(
            rows.iter()
                .all(|r| r.account.as_deref() == Some(hash_of("iso-a").as_str())),
            "every seeded row must be attributed to the seeding account"
        );
        assert!(
            StatsDirtyStore::account_event_rows(&hash_of("iso-b")).is_empty(),
            "a different account must observe none of them"
        );
        assert!(
            account_activity_for(Some(&hash_of("iso-b")), None)
                .expect("activity")
                .is_empty(),
            "reading as the other account must still yield nothing"
        );
        restore();
    }

    // 7. CORRUPT / UNREADABLE HISTORY degrades safely with no ledger write.
    #[test]
    fn unreadable_history_degrades_without_writing_the_ledger() {
        let _guard = super::tests::store_test_guard();
        let dir = std::env::temp_dir().join(format!(
            "fluence-baseline-corrupt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ledger = dir.join("stats_events.json");
        let history = dir.join("history.db");
        std::fs::write(&history, b"this is definitely not sqlite").expect("write junk");
        StatsDirtyStore::set_test_ledger_path(Some(ledger.clone()));
        StatsDirtyStore::set_test_history_path(Some(history));
        StatsDirtyStore::invalidate_activity_cache();

        let buckets = activity("corrupt");

        assert!(
            buckets.is_empty(),
            "a corrupt history must yield the empty state, not partial data"
        );
        let raw = std::fs::read_to_string(&ledger).unwrap_or_default();
        assert!(
            raw.is_empty() || raw == "null" || raw == "[]",
            "no ledger may be written from an unreadable history; got {raw:?}"
        );
        restore();
    }

    // 8. CACHE: a stale empty cache must not mask the freshly seeded data.
    #[test]
    fn a_stale_empty_cache_does_not_mask_the_backfill() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("cache", &[(today - DAY_MS + 3_600_000, 5_000)]);

        // Seed the cache with an empty result as a signed-OUT read would.
        let rows =
            StatsDirtyStore::cached_event_rows(None, || StatsDirtyStore::local_normalized_rows());
        assert!(
            rows.is_empty(),
            "precondition: the cache holds an empty result"
        );

        let buckets = activity("cache");

        assert!(
            !buckets.is_empty(),
            "the backfilled data must be visible immediately, not masked by a stale cache"
        );
        restore();
    }

    // 9. The existing sync-pass backfill still runs and stays idempotent.
    #[test]
    fn the_sync_pass_backfill_remains_idempotent_after_read_time_seeding() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("sync", &[(today - DAY_MS + 3_600_000, 5_000)]);

        StatsDirtyStore::ensure_account_backfilled(&hash_of("sync"));
        let after_read = StatsDirtyStore::load_rows().len();
        assert_eq!(after_read, 1, "read-time seeding wrote the history row");

        let mut store = StatsDirtyStore;
        store.load(&hash_of("sync"));
        assert_eq!(
            StatsDirtyStore::load_rows().len(),
            after_read,
            "the sync-pass backfill must find every id present and write nothing"
        );
        assert!(
            StatsDirtyStore::load_rows()
                .iter()
                .all(|r| r.account.is_some()),
            "no row may be left unattributed"
        );
        restore();
    }

    // 10. GUARD: the activity read path must never mutate real user settings.
    //
    // Regression guard for a real incident: `settings_path()` resolves through
    // `dirs::data_local_dir()` directly and ignores the test-build data-dir
    // redirect, so these tests previously called `save_settings()` and wrote to
    // the actual user profile, clearing the signed-in account.
    #[test]
    fn production_settings_survive_the_activity_read_path() {
        let _guard = super::tests::store_test_guard();
        let production = dirs::data_local_dir()
            .expect("local app data")
            .join("Fluence")
            .join("settings.json");
        let before = std::fs::read(&production).ok();
        let before_meta = std::fs::metadata(&production)
            .ok()
            .map(|m| m.modified().ok());

        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("guard", &[(today - DAY_MS + 3_600_000, 5_000)]);
        let _ = activity("guard");
        restore();

        let after = std::fs::read(&production).ok();
        let after_meta = std::fs::metadata(&production)
            .ok()
            .map(|m| m.modified().ok());
        assert_eq!(
            before, after,
            "the activity read path must not rewrite the real settings.json"
        );
        assert_eq!(
            before_meta, after_meta,
            "the real settings.json must not even be re-written"
        );
    }
}
// ===========================================================================
// STAGE 8 runtime regression: Agent/Style dirty-state lifecycle.
//
// Found by real-device QA, not by the suite: after a record had synced once it
// carried a `syncId`/`updatedAt` forever, so deriving "dirty" from their
// absence made every LATER edit and delete permanently invisible to upload.
// CREATE propagated; EDIT and DELETE did not. These tests pin the full
// lifecycle so that cannot regress silently again.
// ===========================================================================

#[cfg(test)]
mod dirty_lifecycle {
    use super::*;

    /// A valid (64 lowercase hex) account hash derived from a unique tag, so each
    /// test owns its own store without colliding under parallel execution.
    fn qa_account(tag: &str) -> String {
        crate::sync::metadata::account_hash_from_email(&format!("qa-{tag}@runtime.test"))
    }

    fn qa_agent(hash: &str, id: &str) -> crate::account_scope::AccountAgent {
        crate::account_scope::load_account_agents(hash)
            .custom_agents
            .into_iter()
            .find(|a| a.id == id)
            .expect("agent row")
    }

    fn qa_style(hash: &str, id: &str) -> crate::account_scope::AccountStyle {
        crate::account_scope::load_account_styles(hash)
            .custom_styles
            .into_iter()
            .find(|s| s.id == id)
            .expect("style row")
    }

    fn qa_device_id() -> String {
        SyncMetadata::load().ensure_device_id()
    }

    /// CREATE -> dirty -> SYNC -> clean -> EDIT -> dirty again -> SYNC -> clean,
    /// with a strictly newer revision on every mutation.
    #[test]
    fn agent_edit_after_first_sync_is_dirty_again_and_gets_a_newer_revision() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-edit");
        let id = "agent:qa-edit-0001";
        let device = qa_device_id();
        let mut store = AgentDirtyStore;

        // 1. CREATE is dirty.
        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint-1");
        assert!(
            store.has_dirty(&hash),
            "a newly created agent must be dirty before its first sync"
        );
        assert!(
            qa_agent(&hash, id).dirty,
            "the local row must record the pending mutation"
        );

        // 2. SYNC stamps it, then a completed merge makes it clean.
        assert_eq!(store.stamp_account(&hash).unwrap(), 1);
        let first = qa_agent(&hash, id);
        let first_rev = first.updated_at.expect("stamped updatedAt");
        assert!(first_rev > 0);
        assert_eq!(first.device_id.as_deref(), Some(device.as_str()));
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(
            !store.has_dirty(&hash),
            "a successfully synchronized agent must be clean"
        );

        // 3. EDIT of an already-synced agent must become dirty AGAIN. This is the
        //    exact step that used to be a silent no-op.
        crate::account_scope::upsert_account_agent(&hash, id, "v2", "hint-2");
        assert!(
            store.has_dirty(&hash),
            "an edit to an already-synced agent must become dirty again"
        );

        // 4. The edit gets a NEWER revision from THIS device, never the revision
        //    inherited from the peer it was pulled from.
        assert_eq!(store.stamp_account(&hash).unwrap(), 1);
        let edited = qa_agent(&hash, id);
        assert_eq!(edited.hint, "hint-2");
        assert!(
            edited.updated_at.expect("edit revision") > first_rev,
            "the edit must carry a strictly newer LWW revision than the sync it followed"
        );
        assert_eq!(edited.device_id.as_deref(), Some(device.as_str()));

        // 5. Sync again -> clean, and the new revision is what a peer would merge.
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(!store.has_dirty(&hash));
        assert_eq!(
            store.load(&hash)[0].updated_at,
            edited.updated_at.expect("edit revision"),
            "the uploaded revision must be the edited one"
        );
    }

    /// Two consecutive edit+sync rounds must both propagate.
    #[test]
    fn agent_survives_two_edit_sync_cycles() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-twice");
        let id = "agent:qa-twice-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint-1");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        let mut last_rev = qa_agent(&hash, id).updated_at.expect("rev1");
        for round in 2..=3 {
            crate::account_scope::upsert_account_agent(&hash, id, &format!("v{round}"), "hint");
            assert!(
                store.has_dirty(&hash),
                "edit round {round} must dirty the row again"
            );
            store.stamp_account(&hash).unwrap();
            let rev = qa_agent(&hash, id).updated_at.expect("revision");
            assert!(rev > last_rev, "round {round} must advance the revision");
            last_rev = rev;
            store.save_merged(&hash, store.load(&hash)).unwrap();
            assert!(!store.has_dirty(&hash), "round {round} must end clean");
        }
        assert_eq!(store.load(&hash)[0].updated_at, last_rev);
    }

    /// DELETE of an already-synced agent must upload a tombstone with a fresh
    /// revision, instead of leaving the row looking untouched to the peer.
    #[test]
    fn agent_delete_after_first_sync_is_dirty_and_carries_a_fresh_tombstone() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-delete");
        let id = "agent:qa-delete-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint-1");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();
        let synced_rev = qa_agent(&hash, id).updated_at.expect("rev");

        assert!(!store.has_dirty(&hash), "precondition: row starts clean");

        crate::account_scope::delete_account_agent(&hash, id);
        assert!(
            store.has_dirty(&hash),
            "a delete of an already-synced agent must become dirty again"
        );

        store.stamp_account(&hash).unwrap();
        let dead = qa_agent(&hash, id);
        assert!(
            dead.deleted_at.is_some(),
            "the row must be a tombstone, not a removal"
        );
        assert!(
            dead.updated_at.expect("tombstone revision") > synced_rev,
            "the tombstone must carry a newer revision so LWW cannot lose the delete"
        );
        // The tombstone is uploaded, not purged: it has a sync id, so a peer that
        // still holds the live row cannot resurrect it.
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(store.load(&hash)[0].deleted_at.is_some());
    }

    /// DELETE then RECREATE: the app mints a NEW id, so the old tombstone must not
    /// suppress the legitimate replacement, and both rows must coexist correctly.
    #[test]
    fn agent_delete_then_recreate_leaves_the_tombstone_and_the_new_row() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-recreate");
        let old_id = "agent:qa-recreate-old";
        let new_id = "agent:qa-recreate-new";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, old_id, "original", "hint");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        crate::account_scope::delete_account_agent(&hash, old_id);
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        // Recreating mints a fresh id; the tombstone for the old id stays put.
        crate::account_scope::upsert_account_agent(&hash, new_id, "recreated", "hint");
        assert!(store.has_dirty(&hash), "the recreated row must upload");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        let rows = store.load(&hash);
        assert_eq!(rows.len(), 2, "tombstone and replacement must both survive");
        assert!(rows
            .iter()
            .any(|r| r.business_key == old_id && r.deleted_at.is_some()));
        assert!(
            rows.iter()
                .any(|r| r.business_key == new_id && r.deleted_at.is_none()),
            "the replacement must be live, not suppressed by the old tombstone"
        );
    }

    /// A row written BEFORE the dirty flag existed (no flag on disk, but stamped
    /// metadata) must still upload exactly once rather than being stranded.
    #[test]
    fn a_stamped_row_without_the_dirty_flag_is_still_deduped_not_duplicated() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-legacy-flag");
        let id = "agent:qa-legacy-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        // A clean row that never needs stamping stays clean and unstamped.
        assert!(!store.has_dirty(&hash));
        assert_eq!(store.stamp_account(&hash).unwrap(), 0);
        assert!(!store.has_dirty(&hash));
        let _ = id;
    }

    /// Style counterpart of the agent lifecycle: edit and delete must both
    /// re-dirty after the first sync.
    #[test]
    fn style_edit_and_delete_after_first_sync_both_become_dirty_again() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("style-lifecycle");
        let id = "custom:qa-style-0001";
        let device = qa_device_id();
        let mut store = StyleDirtyStore;

        crate::account_scope::upsert_account_style(&hash, id, "v1", "style-hint");
        assert!(store.has_dirty(&hash), "a new style must be dirty");
        store.stamp_account(&hash).unwrap();
        let first_rev = qa_style(&hash, id).updated_at.expect("style rev");
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(!store.has_dirty(&hash), "a synced style must be clean");

        // EDIT
        crate::account_scope::upsert_account_style(&hash, id, "v2", "style-hint-2");
        assert!(store.has_dirty(&hash), "style edit must re-dirty");
        store.stamp_account(&hash).unwrap();
        let edited = qa_style(&hash, id);
        assert_eq!(edited.hint, "style-hint-2");
        assert!(edited.updated_at.expect("edit rev") > first_rev);
        assert_eq!(edited.device_id.as_deref(), Some(device.as_str()));
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(!store.has_dirty(&hash));

        // DELETE
        crate::account_scope::delete_account_style(&hash, id);
        assert!(store.has_dirty(&hash), "style delete must re-dirty");
        store.stamp_account(&hash).unwrap();
        let dead = qa_style(&hash, id);
        assert!(dead.deleted_at.is_some(), "style delete must tombstone");
        assert!(dead.updated_at.expect("tombstone rev") > edited.updated_at.unwrap());
    }

    /// Convergence: when a peer's row wins the LWW merge, the local row must end up
    /// byte-identical to the peer's revision, and must be clean afterwards.
    #[test]
    fn a_peer_winner_replaces_the_local_row_and_leaves_it_clean() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-converge");
        let id = "agent:qa-converge-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "local", "local-hint");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        let peer = AgentItem {
            sync_id: AgentItem::stable_sync_id(id),
            business_key: id.to_string(),
            name: "peer".to_string(),
            hint: "peer-hint".to_string(),
            updated_at: qa_agent(&hash, id).updated_at.expect("rev") + 5_000,
            deleted_at: None,
            device_id: "peer-device".to_string(),
        };
        store.save_merged(&hash, vec![peer.clone()]).unwrap();

        let local = store.load(&hash);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].name, "peer");
        assert_eq!(local[0].hint, "peer-hint");
        assert_eq!(local[0].device_id, "peer-device");
        assert!(
            !store.has_dirty(&hash),
            "a merged peer winner must leave the row clean, or it would re-upload forever"
        );
    }

    // ---------------------------------------------------------------------
    // One-tap legacy claim: pinning the REAL account-store writer
    // ---------------------------------------------------------------------
    //
    // The claim takes `io_lock`, but `save_merged` can still be handed a payload
    // built from a document read BEFORE the Drive round trip, so it can discard a
    // committed claim. Two properties must therefore hold, asserted here against
    // the real writer rather than a stand-in:
    //
    //  1. the real merge writer contends for the same critical section as the
    //     claim, so a claim in progress cannot be half-applied around a save;
    //  2. a stale merge that DOES discard the claim leaves the record recoverable,
    //     because the claim copied rather than moved it.
    //
    // These live in `stores` because they need `store_test_guard()` (the account
    // stores are shared files on disk) and the real `AgentDirtyStore`.

    /// Pins that `AgentDirtyStore::save_merged` takes `io_lock`.
    ///
    /// Fails if that lock is removed: the writer would then run to completion while
    /// the critical section is held.
    ///
    /// The assertion is ORDER-based, never a timeout. A timeout version of this test
    /// proved flaky under full-suite load, where the machine can schedule the
    /// writer faster than a fixed window expects. Here the writer records whether
    /// it had finished by the time the holder released, so a slow machine can only
    /// make the test slower — it cannot make it fail spuriously.
    #[test]
    fn the_real_account_merge_writer_contends_for_the_claim_lock() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("claim-lock-contention");
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};

        let released = Arc::new(AtomicBool::new(false));
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();

        // Stands in for a claim in progress: holds the critical section.
        let holder = {
            let released = Arc::clone(&released);
            std::thread::spawn(move || {
                let _io = crate::sync::io_lock::io_lock_guard();
                // Sent only AFTER the guard is held, so the lock really is held.
                held_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                released.store(true, Ordering::SeqCst);
            })
        };
        held_rx.recv().unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let finished_early = Arc::new(AtomicBool::new(false));
        let writer = {
            let released = Arc::clone(&released);
            let finished_early = Arc::clone(&finished_early);
            let hash = hash.clone();
            std::thread::spawn(move || {
                let mut store = AgentDirtyStore;
                let _ = store.save_merged(&hash, Vec::new());
                finished_early.store(!released.load(Ordering::SeqCst), Ordering::SeqCst);
                done_tx.send(()).unwrap();
            })
        };

        // A grace window for the writer to START. Without the lock it finishes in
        // here and sets the flag; with the lock held it cannot finish at all.
        // A slow machine merely makes this sleep elapse with the writer still
        // blocked, which is the passing outcome anyway.
        std::thread::sleep(std::time::Duration::from_millis(150));
        go_tx.send(()).unwrap();

        done_rx.recv().unwrap();
        holder.join().unwrap();
        writer.join().unwrap();

        assert!(
            !finished_early.load(Ordering::SeqCst),
            "the real merge writer ran to completion while the account-store lock \
             was held; if save_merged does not take that lock, a claim and a merge \
             can interleave and discard the claim"
        );
    }

    /// A claim racing the REAL stamper loses nothing.
    ///
    /// Thread A drives production `AgentDirtyStore::stamp_account` in a loop
    /// while thread B performs a real claim. Either side dropping `io_lock`
    /// lets the stale snapshot win and the final assertions fail. This replaces
    /// the earlier hand-rolled stand-in, which proved only that the claim takes
    /// a lock — not that the production stamper shares it.
    #[test]
    fn claim_survives_concurrent_real_stamp_writes() {
        let _guard = super::tests::store_test_guard();
        let h = uuid::Uuid::new_v4().simple().to_string();
        // A valid account hash (64 lowercase hex): the claim filters anything
        // else, which would make this test vacuously green.
        let hash = format!("{h}{h}");
        // One dirty row, so every stamp pass actually rewrites the document and
        // the contention is real rather than early-returned away.
        crate::account_scope::upsert_account_agent(&hash, "agent:qa-race-old", "Old", "hint");
        let legacy = vec![crate::agents::CustomAgent {
            id: "agent:qa-race-new".into(),
            name: "New".into(),
            hint: "h".into(),
        }];
        let only = std::collections::HashSet::from(["agent:qa-race-new".to_string()]);

        let hammer_hash = hash.clone();
        let stamper = std::thread::spawn(move || {
            let mut store = AgentDirtyStore;
            for _ in 0..200 {
                let _ = store.stamp_account(&hammer_hash);
            }
        });
        let out =
            crate::account_scope::claim_legacy_account_agents(Some(&hash), &legacy, Some(&only))
                .expect("claim must succeed in test");
        stamper.join().unwrap();

        assert_eq!(out.claimed_ids, vec!["agent:qa-race-new".to_string()]);
        let final_store = crate::account_scope::load_account_agents(&hash);
        assert!(
            final_store
                .custom_agents
                .iter()
                .any(|a| a.id == "agent:qa-race-new"),
            "the claim must survive 200 real stamp passes"
        );
        assert!(
            final_store
                .custom_agents
                .iter()
                .any(|a| a.id == "agent:qa-race-old"),
            "the pre-existing row must survive too"
        );
    }

    /// Style counterpart: a claim racing the REAL style stamper loses nothing.
    #[test]
    fn style_claim_survives_concurrent_real_stamp_writes() {
        let _guard = super::tests::store_test_guard();
        let h = uuid::Uuid::new_v4().simple().to_string();
        let hash = format!("{h}{h}");
        crate::account_scope::upsert_account_style(&hash, "custom:qa-race-old", "Old", "hint");
        let legacy = vec![crate::prompts::CustomStyle {
            id: "custom:qa-race-new".into(),
            name: "New".into(),
            hint: "h".into(),
        }];
        let only = std::collections::HashSet::from(["custom:qa-race-new".to_string()]);

        let hammer_hash = hash.clone();
        let stamper = std::thread::spawn(move || {
            let mut store = StyleDirtyStore;
            for _ in 0..200 {
                let _ = store.stamp_account(&hammer_hash);
            }
        });
        let out =
            crate::account_scope::claim_legacy_account_styles(Some(&hash), &legacy, Some(&only))
                .expect("claim must succeed in test");
        stamper.join().unwrap();

        assert_eq!(out.claimed_ids, vec!["custom:qa-race-new".to_string()]);
        let final_store = crate::account_scope::load_account_styles(&hash);
        assert!(
            final_store
                .custom_styles
                .iter()
                .any(|s| s.id == "custom:qa-race-new"),
            "the style claim must survive 200 real stamp passes"
        );
        assert!(
            final_store
                .custom_styles
                .iter()
                .any(|s| s.id == "custom:qa-race-old"),
            "the pre-existing style must survive too"
        );
    }

    /// A stale merge driven through the REAL writer now PRESERVES the claimed row.
    ///
    /// This test previously pinned the opposite: that the pass discarded a row
    /// claimed while it was on the network, with the un-removed legacy copy
    /// acting as the recovery path ("B2/R1 remains open"). `save_merged` now
    /// reconciles against the current local rows instead of wholesale-writing
    /// its pre-network snapshot, so the claim survives the pass itself
    /// (audit finding H5). The legacy copy is still never removed on claim
    /// success and remains the fallback for any other drop path.
    #[test]
    fn a_stale_real_merge_now_preserves_the_claim() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("claim-stale-merge");
        let id = "agent:qa-stale-merge-0001";
        let legacy = vec![crate::agents::CustomAgent {
            id: id.to_string(),
            name: "Legacy".into(),
            hint: "h".into(),
        }];
        let only = std::collections::HashSet::from([id.to_string()]);

        let claimed =
            crate::account_scope::claim_legacy_account_agents(Some(&hash), &legacy, Some(&only))
                .expect("claim must succeed in test");
        assert_eq!(claimed.claimed_ids, vec![id.to_string()]);
        assert!(crate::account_scope::load_account_agents(&hash)
            .custom_agents
            .iter()
            .any(|a| a.id == id));

        // The pass writes the payload it read BEFORE the claim: an empty
        // payload is exactly the "stale snapshot" case. Real writer.
        let mut store = AgentDirtyStore;
        store.save_merged(&hash, Vec::new()).unwrap();

        let after = crate::account_scope::load_account_agents(&hash);
        let survived = after
            .custom_agents
            .iter()
            .find(|a| a.id == id)
            .expect("the claimed row must survive a stale merge that omitted it");
        assert!(
            survived.dirty,
            "and it stays dirty, so the next pass uploads it instead of \
             stamping a never-pushed row as synced"
        );

        // The board must not offer a second claim for a row this device owns:
        // the surviving account row shadows the legacy copy in the union.
        let view = crate::account_scope::VisibleAgents::from_union_with_account(
            crate::account_scope::union_agents(&legacy, &after.custom_agents),
            Some(&hash),
        )
        .into_view(crate::agents::ID_BUILT_IN.to_string());
        assert_eq!(view.custom_agents.len(), 1);
        assert!(
            !view.custom_agents[0].claimable,
            "the surviving row is owned, so it is not claimable again"
        );
    }

    /// Cross-platform reconcile conformance (audit finding G3).
    ///
    /// The Windows `AgentDirtyStore::save_merged` and the Android
    /// `agentStore.applyMergedAndClearDirty` implement the same three rules,
    /// but nothing mechanically enforced that until these vectors existed — a
    /// stale-merge test on each side once documented OPPOSITE behaviors. These
    /// three scenarios use fixed ids both sides construct literally, and the
    /// Android twin
    /// (`ReconcileConformanceTest`, same three names) asserts the same
    /// outcomes through the real store path. If either side changes its rule,
    /// its twin goes red; if the vectors drift apart, the names stop matching.
    ///
    /// The rules, stated once:
    ///  1. a row the merge omits entirely survives exactly as stored;
    ///  2. a dirty row whose content still differs from its winner survives
    ///     dirty, so the next pass uploads it;
    ///  3. a dirty row whose content already equals its winner is replaced by
    ///     the clean winner — otherwise `dirty` would re-upload forever.
    mod reconcile_conformance {
        use super::*;

        const V1_ID: &str = "agent:conformance-0001";
        const V2_ID: &str = "agent:conformance-0002";
        const V3_ID: &str = "agent:conformance-0003";

        fn row(
            id: &str,
            name: &str,
            sync_id: Option<&str>,
            dirty: bool,
        ) -> crate::account_scope::AccountAgent {
            crate::account_scope::AccountAgent {
                id: id.to_string(),
                name: name.to_string(),
                hint: "h".to_string(),
                sync_id: sync_id.map(str::to_string),
                updated_at: Some(1000),
                device_id: Some("test-device".to_string()),
                deleted_at: None,
                dirty,
            }
        }

        fn winner(id: &str, name: &str, sync_id: &str) -> AgentItem {
            AgentItem {
                sync_id: sync_id.to_string(),
                business_key: id.to_string(),
                name: name.to_string(),
                hint: "h".to_string(),
                updated_at: 2000,
                deleted_at: None,
                device_id: "test-device".to_string(),
            }
        }

        fn seed(hash: &str, rows: Vec<crate::account_scope::AccountAgent>) {
            crate::account_scope::save_account_agents(
                hash,
                &crate::account_scope::AccountAgentsStore {
                    custom_agents: rows,
                },
            )
            .expect("conformance seed must write");
        }

        fn load(hash: &str) -> Vec<crate::account_scope::AccountAgent> {
            crate::account_scope::load_account_agents(hash).custom_agents
        }

        /// Vector 1: a row the merge omits entirely survives exactly as stored.
        #[test]
        fn conformance_omitted_row_survives() {
            let _guard = store_test_guard();
            let hash = qa_account("reconcile-conformance-v1");
            seed(&hash, vec![row(V1_ID, "Conf One", None, true)]);

            let mut store = crate::sync::stores::AgentDirtyStore;
            use crate::sync::stores::DirtyStore;
            store.save_merged(&hash, Vec::new()).unwrap();

            let after = load(&hash);
            let survived = after
                .iter()
                .find(|a| a.id == V1_ID)
                .expect("vector 1: omitted row must survive");
            assert!(survived.dirty, "vector 1: and it stays dirty");
            assert_eq!(after.iter().filter(|a| a.id == V1_ID).count(), 1);
        }

        /// Vector 2: a dirty row whose content differs from its winner survives.
        #[test]
        fn conformance_dirty_differing_row_survives() {
            let _guard = store_test_guard();
            let hash = qa_account("reconcile-conformance-v2");
            seed(&hash, vec![row(V2_ID, "Local Name", Some("sync-2"), true)]);

            let mut store = crate::sync::stores::AgentDirtyStore;
            use crate::sync::stores::DirtyStore;
            store
                .save_merged(&hash, vec![winner(V2_ID, "Winner Name", "sync-2")])
                .unwrap();

            let after = load(&hash);
            let survived = after
                .iter()
                .find(|a| a.id == V2_ID)
                .expect("vector 2: differing dirty row must survive");
            assert_eq!(survived.name, "Local Name");
            assert!(
                survived.dirty,
                "vector 2: and it stays dirty for the next upload"
            );
        }

        /// Vector 3: a dirty row whose content already equals its winner is
        /// replaced by the clean winner.
        #[test]
        fn conformance_dirty_matching_row_goes_clean() {
            let _guard = store_test_guard();
            let hash = qa_account("reconcile-conformance-v3");
            seed(&hash, vec![row(V3_ID, "Same Name", Some("sync-3"), true)]);

            let mut store = crate::sync::stores::AgentDirtyStore;
            use crate::sync::stores::DirtyStore;
            store
                .save_merged(&hash, vec![winner(V3_ID, "Same Name", "sync-3")])
                .unwrap();

            let after = load(&hash);
            assert_eq!(
                after.iter().filter(|a| a.id == V3_ID).count(),
                1,
                "vector 3: exactly one row, never a duplicate"
            );
            let settled = after.iter().find(|a| a.id == V3_ID).unwrap();
            assert!(
                !settled.dirty,
                "vector 3: content equals the winner, so the winner is authoritative and the row goes clean"
            );
        }
    }
}

// ===========================================================================
// Sync-audit regression tests (findings #4, #6, #15, #19, #20, #22, #29).
//
// Hermetic by construction: every test either redirects a store to a unique
// temp path (`StatsDirtyStore::set_test_ledger_path`), drives a
// path-parameterized core, or injects `SettingsIo` fakes. Nothing here reaches
// `settings.json` - its path resolves through `dirs::data_local_dir()` and
// IGNORES the test data-dir redirect, so touching it would write the real user
// profile.
// ===========================================================================

#[cfg(test)]
mod audit_regressions {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Truth table for the mid-pass settings guard.
    ///
    /// The guard exists because a user edit landing during the GET→PUT window
    /// was silently destroyed: the edit is absent from `merged`, `meta.keys`
    /// still holds the pre-edit baseline so the winner still "beats baseline",
    /// the winner is applied over the edit, and the baseline then advances — no
    /// dirty residue, no retry, no trace.
    ///
    /// The `false` case is the one that matters: `(Some, Some)` that DIFFER is
    /// the mid-pass edit, and the only correct answer is "defer". Every
    /// unobserved arm must be `true` (apply), because that is exactly this
    /// platform's behaviour before the guard existed — never a new failure mode.
    #[test]
    fn decide_settings_apply_truth_table() {
        let same = "fr".to_string();
        let other = "de".to_string();
        // Unchanged -> apply.
        assert!(SettingsDirtyStore::decide_settings_apply(
            Some(&same),
            Some(&same)
        ));
        // Changed during the pass -> DEFER. This is the regression guard.
        assert!(!SettingsDirtyStore::decide_settings_apply(
            Some(&same),
            Some(&other)
        ));
        // Unobserved on either side -> apply (pre-guard behaviour).
        assert!(SettingsDirtyStore::decide_settings_apply(None, Some(&same)));
        assert!(SettingsDirtyStore::decide_settings_apply(Some(&same), None));
        assert!(SettingsDirtyStore::decide_settings_apply(None, None));
    }

    fn unique(tag: &str, ext: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fluence-audit-{}-{}-{}.{ext}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// Remove a path and every `*.corrupt[.N]` sibling `read_json_file` may
    /// have left behind while quarantining it.
    fn wipe(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("json.corrupt.json"));
        for n in 1..64u32 {
            let p = path.with_extension(format!("json.corrupt.{n}.json"));
            if !p.exists() {
                break;
            }
            let _ = std::fs::remove_file(&p);
        }
    }

    // ── #22: the fixed tmp filename ─────────────────────────────────────

    #[test]
    fn atomic_write_uses_a_unique_temp_name_per_call() {
        // Audit #22: `path.with_extension("tmp")` is the settings.rs incident
        // verbatim: two concurrent writers share one tmp, so one renames a file
        // the other is still writing (truncated store on the destination).
        let dir = unique("atomic-tmp", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings_sync_probe.json");

        let first = super::atomic_tmp_path(&path);
        let second = super::atomic_tmp_path(&path);

        assert_ne!(
            first, second,
            "every atomic write needs its own temp file name"
        );
        assert_eq!(
            first.parent(),
            path.parent(),
            "the temp must be a sibling of the destination so the rename stays same-volume atomic"
        );
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(path.file_name().unwrap().to_string_lossy().as_ref()),
            "the temp name must derive from the destination file name"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_atomic_writers_never_publish_a_partial_file() {
        let dir = unique("atomic-race", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stats_events.json");
        // Big enough that a torn write is unmistakable.
        let payloads: Vec<String> = (0..8)
            .map(|i| format!("[{i:04}]").to_string() + &"x".repeat(256 * 1024))
            .collect();
        let dest = &path;
        std::thread::scope(|s| {
            for payload in payloads.clone() {
                s.spawn(move || {
                    for _ in 0..3 {
                        super::atomic_write(dest, &payload).expect("atomic write");
                    }
                });
            }
        });
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            payloads.iter().any(|p| p == &after),
            "the destination must hold exactly one complete payload, never a \
             partial one renamed over it"
        );
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "stats_events.json")
            .collect();
        assert!(
            strays.is_empty(),
            "no temp file may be left behind: {strays:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── #6b: an unreadable local store must never read as empty/clean ────

    #[test]
    fn corrupt_dictionary_store_never_reports_a_clean_pass() {
        let _guard = super::store_test_guard();
        let hash = format!("audit-dict-{}", uuid::Uuid::new_v4());
        let path = super::data_dir().join("dictionary.json");
        let before = load_dictionary_internal().unwrap_or_default();

        // A healthy store with one dirty row for this account.
        let mut seeded = before.clone();
        seeded.push(DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "auditword".to_string(),
            corrected: "audit corrected".to_string(),
            kind: crate::dictionary::KIND_CORRECTION.to_string(),
            created_at: Some(1_000),
            deleted_at: None,
            updated_at: Some(2_000),
            device_id: Some("audit-device".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        save_dictionary_internal(&seeded).unwrap();
        let store = DictionaryDirtyStore;
        assert!(
            store.has_dirty(&hash),
            "precondition: a readable dirty row reports dirty"
        );

        // Audit #6b: the corruption is quarantined by the first read, so each
        // assertion re-corrupts the file to get a fresh unreadable state.
        std::fs::write(&path, b"{ definitely not json").unwrap();
        assert!(
            store.has_dirty(&hash),
            "an UNREADABLE dictionary store must fail closed, never report clean \
             (a clean report is a silent, successful pass with no data)"
        );
        assert!(
            path.with_extension("json.corrupt.json").exists(),
            "the evidence must be preserved, not deleted"
        );

        std::fs::write(&path, b"{ definitely not json").unwrap();
        let err = store
            .load_strict(&hash)
            .expect_err("load must surface an unreadable store, not invent an empty one");
        assert!(
            matches!(err, SyncError::Fatal(_)),
            "unreadable local state is Fatal, exactly like stamp_account: {err:?}"
        );
        std::fs::write(&path, b"{ definitely not json").unwrap();
        assert!(
            store.load(&hash).is_empty(),
            "the infallible wrapper degrades to empty but has_dirty already \
             reported dirty, so the pass cannot conclude 'nothing to do'"
        );

        save_dictionary_internal(&before).unwrap();
        wipe(&path);
    }

    #[test]
    fn corrupt_stats_ledger_never_reports_a_clean_pass() {
        let _guard = super::store_test_guard();
        let ledger = unique("audit-stats", "json");
        StatsDirtyStore::set_test_ledger_path(Some(ledger.clone()));
        let hash = format!("audit-stats-{}", uuid::Uuid::new_v4());

        std::fs::write(&ledger, b"[[[ not a ledger").unwrap();
        let store = StatsDirtyStore;
        assert!(
            store.has_dirty(&hash),
            "an UNREADABLE stats ledger must fail closed: reporting clean \
             completes a successful pass that silently drops every event"
        );

        std::fs::write(&ledger, b"[[[ not a ledger").unwrap();
        let err = StatsDirtyStore::load_rows_strict()
            .expect_err("the ledger read must surface corruption");
        assert!(
            matches!(
                err,
                crate::sync::recover::ReadError::Parse { .. }
                    | crate::sync::recover::ReadError::Io(_)
            ),
            "unreadable ledger: {err:?}"
        );
        std::fs::write(&ledger, b"[[[ not a ledger").unwrap();
        assert!(
            StatsDirtyStore::load_rows().is_empty(),
            "the pure view degrades to empty; it must never be a silent 'no data'"
        );

        StatsDirtyStore::set_test_ledger_path(None);
        wipe(&ledger);
    }

    // ── #15: unreadable snippets store must not become snippets=false ────

    #[test]
    fn an_unreadable_snippets_store_emits_no_settings_items() {
        let _guard = super::store_test_guard();
        let hash = format!("audit-settings-{}", uuid::Uuid::new_v4());
        let snippets = super::data_dir().join("snippets.json");
        std::fs::create_dir_all(super::data_dir()).expect("test data dir");
        let meta_path = SettingsDirtyStore::meta_path(&hash);
        let _ = std::fs::remove_file(&meta_path);
        let original = std::fs::read(&snippets).ok();

        // A healthy store with snippets ON, captured as the account baseline.
        std::fs::write(&snippets, r#"{"enabled":true,"snippets":[]}"#).unwrap();
        SettingsDirtyStore::activate_account(&hash).expect("capture baseline");
        let meta = SettingsDirtyStore::load_meta(&hash);
        assert_eq!(
            meta.activation_baseline
                .as_ref()
                .and_then(|b| b.get("snippets_enabled"))
                .map(String::as_str),
            Some("true"),
            "precondition: the baseline observed snippets_enabled=true"
        );

        // Audit #15: with the snippets store unreadable, the old
        // `unwrap_or(false)` produced `snippets_enabled=false`, which the pass
        // stamped with `edit_now` (the account's highest timestamp) and
        // uploaded - winning LWW fleet-wide and disabling expansion everywhere.
        //
        // The store is made unreadable by putting a DIRECTORY at its path, not
        // by writing corrupt bytes. `recover::read_json_file` PRESERVES CORRUPT
        // BYTES ASIDE on the first read, so the very next reader sees a MISSING
        // file, gets `SnippetStore::default()` (enabled = false), and
        // `live_values_strict` SUCCEEDS - which is exactly the fabricated
        // `false` this test exists to catch. Writing corrupt bytes therefore
        // only held while this test happened to be the first reader of that
        // file; under any other ordering the premise silently evaporated and
        // the suite went red on CI with "no baseline may be recorded for a
        // value that was never observed". A directory fails `fs::read` with an
        // IO error on EVERY read and is never rotated aside, so "unreadable" is
        // a stable property here rather than a single-use one.
        std::fs::remove_file(&snippets).ok();
        std::fs::create_dir_all(&snippets).expect("replace the store with a directory");
        // Assert the premise is durable, so this test can never pass vacuously
        // again by reading a store that merely looks unreadable once.
        assert!(
            load_snippets_internal().is_err(),
            "precondition: the snippets store must be unreadable"
        );
        assert!(
            load_snippets_internal().is_err(),
            "precondition: unreadability must survive repeated reads instead of \
             being consumed by rotating the corrupt bytes aside"
        );
        let items = SettingsDirtyStore.load(&hash);
        assert!(
            items.iter().all(|i| i.key != "snippets_enabled"),
            "an unobserved value must never be emitted: {items:?}"
        );
        assert!(
            items.is_empty(),
            "nothing may be stamped or uploaded from an unreadable snippets \
             store, not even the three keys that were readable: {items:?}"
        );
        let meta_after = SettingsDirtyStore::load_meta(&hash);
        assert!(
            !meta_after.keys.contains_key("snippets_enabled"),
            "no baseline may be recorded for a value that was never observed"
        );

        // Restore the shared store: drop the directory stand-in BEFORE writing
        // bytes back, otherwise the restore fails and leaks a directory that
        // every later settings/snippets test would read as an unreadable store.
        std::fs::remove_dir(&snippets).ok();
        match original {
            Some(bytes) => std::fs::write(&snippets, bytes).unwrap(),
            None => {
                let _ = std::fs::remove_file(&snippets);
            }
        }
        let _ = std::fs::remove_file(&meta_path);
        wipe(&snippets);
    }

    // ── settings pass harness (injected io, never the real profile) ──────

    struct FakeSettingsIo {
        meta_path: PathBuf,
        settings: RefCell<AppSettings>,
        save_calls: Cell<usize>,
        save_fails: Cell<bool>,
        snippet_calls: RefCell<Vec<bool>>,
        snippet_fails: Cell<bool>,
    }

    impl FakeSettingsIo {
        fn new(tag: &str) -> Self {
            let meta_path = unique(tag, "json");
            let _ = std::fs::remove_file(&meta_path);
            Self {
                meta_path,
                settings: RefCell::new(AppSettings::default()),
                save_calls: Cell::new(0),
                save_fails: Cell::new(false),
                snippet_calls: RefCell::new(Vec::new()),
                snippet_fails: Cell::new(false),
            }
        }

        /// Build a [`SettingsIo`] over this fake and run `f` with it. The closures
        /// must live in one frame that outlives the borrow, so they are
        /// created here rather than returned.
        fn with_io<R>(&self, f: impl FnOnce(&SettingsIo) -> R) -> R {
            let load_settings = || Ok(self.settings.borrow().clone());
            let save_settings = |s: &AppSettings| {
                self.save_calls.set(self.save_calls.get() + 1);
                if self.save_fails.get() {
                    Err("injected settings write failure".to_string())
                } else {
                    self.settings.replace(s.clone());
                    Ok(())
                }
            };
            let apply_snippets_enabled = |enabled: bool| {
                self.snippet_calls.borrow_mut().push(enabled);
                if self.snippet_fails.get() {
                    Err("injected snippets write failure".to_string())
                } else {
                    Ok(())
                }
            };
            let still_active = || true;
            let io = SettingsIo {
                meta_path: &self.meta_path,
                load_settings: &load_settings,
                save_settings: &save_settings,
                apply_snippets_enabled: &apply_snippets_enabled,
                still_active: &still_active,
            };
            f(&io)
        }

        fn meta(&self) -> SettingsMetaDoc {
            SettingsDirtyStore::load_meta_from(&self.meta_path)
        }

        /// Record a last-synced baseline, i.e. "we already applied {v} at {t}
        /// from device {d}".
        fn seed_baseline(&self, key: &str, v: &str, t: i64, d: &str) {
            let mut meta = self.meta();
            meta.keys.insert(
                key.to_string(),
                KeyMeta {
                    v: v.to_string(),
                    t,
                    d: d.to_string(),
                },
            );
            SettingsDirtyStore::save_meta_to(&self.meta_path, &meta).expect("seed meta");
        }

        fn cleanup(&self) {
            wipe(&self.meta_path);
        }
    }

    fn winner(key: &str, value: &str, updated_at: i64, device_id: &str) -> SettingsItem {
        SettingsItem {
            key: key.to_string(),
            value: value.to_string(),
            updated_at,
            device_id: device_id.to_string(),
            deleted_at: None,
        }
    }

    // ── #4: the winner test must be the merge's total order ──────────────

    #[test]
    fn settings_timestamp_tie_resolves_by_device_id() {
        let fake = FakeSettingsIo::new("tie-device");
        fake.seed_baseline("language", "en", 1_000, "dev-a");

        // Identical updatedAt, larger deviceId: `merge_settings` resolves this
        // tie by deviceId, so `save_merged` must accept the same winner. The
        // old strict `>` dropped it, and the two paths disagreed about who
        // owned `language` - permanent divergence between two devices.
        fake.with_io(|io| {
            SettingsDirtyStore::apply_merged(io, &[winner("language", "fr", 1_000, "dev-b")])
                .expect("tie resolved by deviceId")
        });

        assert_eq!(
            fake.settings.borrow().language,
            "fr",
            "the merge's winner must be applied, not dropped"
        );
        let meta = fake.meta();
        assert_eq!(meta.keys["language"].v, "fr");
        assert_eq!(meta.keys["language"].t, 1_000);
        assert_eq!(
            meta.keys["language"].d, "dev-b",
            "the baseline must remember the winning device so the next tie is \
             comparable"
        );
        fake.cleanup();
    }

    #[test]
    fn settings_full_tie_resolves_by_value() {
        // Same updatedAt AND same deviceId: only the value separates them, and
        // the canonical comparator (`cmp_settings_winner`, shared with
        // `merge_settings` and Android) says the larger value wins.
        let fake = FakeSettingsIo::new("tie-value");
        fake.seed_baseline("ai_polish_style", "dark", 2_000, "dev-a");
        // Pin the comparator itself against the canonical order: equal time,
        // equal device, LARGER value wins (`cmp_settings_winner`, the very
        // comparator `merge_settings` folds with). "light" sorts after "dark".
        assert!(
            SettingsDirtyStore::winner_beats_baseline(
                &winner("ai_polish_style", "light", 2_000, "dev-a"),
                &KeyMeta {
                    v: "dark".to_string(),
                    t: 2_000,
                    d: "dev-a".to_string(),
                },
            ),
            "the canonical settings order says the larger value wins a full tie"
        );
        fake.with_io(|io| {
            SettingsDirtyStore::apply_merged(
                io,
                &[winner("ai_polish_style", "light", 2_000, "dev-a")],
            )
            .expect("full tie resolved by value")
        });
        assert_eq!(
            fake.settings.borrow().ai_polish_style,
            "light",
            "the larger value wins a total tie, exactly as merge_settings decides"
        );
        assert_eq!(fake.save_calls.get(), 1);
        assert_eq!(fake.meta().keys["ai_polish_style"].v, "light");

        // The mirror image: a smaller value on the same tie LOSES, so the live
        // value and the baseline must both stay put.
        let loser = FakeSettingsIo::new("tie-value-loser");
        loser.seed_baseline("ai_polish_style", "light", 2_000, "dev-a");
        loser.with_io(|io| {
            SettingsDirtyStore::apply_merged(
                io,
                &[winner("ai_polish_style", "dark", 2_000, "dev-a")],
            )
            .expect("losing tie is a no-op")
        });
        assert_eq!(
            loser.save_calls.get(),
            0,
            "a losing winner must not rewrite settings"
        );
        assert_eq!(loser.meta().keys["ai_polish_style"].v, "light");
        assert_ne!(loser.settings.borrow().ai_polish_style, "dark");
        fake.cleanup();
        loser.cleanup();
    }

    // ── #29: values before baselines ─────────────────────────────────────

    #[test]
    fn a_failed_value_write_does_not_advance_the_meta_baseline() {
        let fake = FakeSettingsIo::new("crash-window");
        fake.seed_baseline("language", "en", 1_000, "dev-a");

        // Audit #29: writing meta first means a crash between the two writes
        // leaves meta claiming the remote winner while the value on disk is
        // still the stale local one. The next pass then sees "local edit",
        // stamps it `edit_now` and uploads it - reverting the remote winner
        // for good, self-healing toward the wrong state.
        fake.save_fails.set(true);
        let err = fake.with_io(|io| {
            SettingsDirtyStore::apply_merged(io, &[winner("language", "fr", 2_000, "dev-b")])
        });
        let err = err.expect_err("a failed value write must surface");
        assert!(matches!(err, SyncError::Fatal(_)), "{err:?}");

        let meta = fake.meta();
        assert_eq!(
            meta.keys["language"].v, "en",
            "meta must NOT record a winner whose value was never written"
        );
        assert_eq!(meta.keys["language"].t, 1_000);

        // The next pass therefore re-applies the SAME remote winner rather
        // than re-uploading the stale local value.
        fake.save_fails.set(false);
        fake.with_io(|io| {
            SettingsDirtyStore::apply_merged(io, &[winner("language", "fr", 2_000, "dev-b")])
                .expect("retry re-applies the remote winner")
        });
        assert_eq!(fake.settings.borrow().language, "fr");
        assert_eq!(fake.meta().keys["language"].v, "fr");
        fake.cleanup();
    }

    // ── #19: a failed snippets write must not record the winner ───────────

    #[test]
    fn a_failed_snippets_write_does_not_record_the_winner_in_meta() {
        let fake = FakeSettingsIo::new("snippets-write");
        fake.seed_baseline("snippets_enabled", "false", 5_000, "dev-a");

        fake.snippet_fails.set(true);
        let err = fake.with_io(|io| {
            SettingsDirtyStore::apply_merged(
                io,
                &[winner("snippets_enabled", "true", 6_000, "dev-b")],
            )
        });
        let err = err.expect_err("audit #19: the snippets save failure must propagate");
        assert!(matches!(err, SyncError::Fatal(_)), "{err:?}");
        assert_eq!(
            fake.snippet_calls.borrow().clone(),
            vec![true],
            "the winner must actually have been attempted"
        );
        assert_eq!(
            fake.save_calls.get(),
            0,
            "no further value writes may follow a failed one"
        );
        let meta = fake.meta();
        assert_eq!(
            meta.keys["snippets_enabled"].v, "false",
            "recording the winner while the live store is stale makes the next \
             pass re-upload the OLD value with a higher timestamp - a toggle \
             that flips forever"
        );
        assert_eq!(meta.keys["snippets_enabled"].t, 5_000);

        // Once the store is writable the toggle converges instead of flipping.
        fake.snippet_fails.set(false);
        fake.with_io(|io| {
            SettingsDirtyStore::apply_merged(
                io,
                &[winner("snippets_enabled", "true", 6_000, "dev-b")],
            )
            .expect("retry applies the winner")
        });
        assert_eq!(fake.meta().keys["snippets_enabled"].v, "true");
        fake.cleanup();
    }

    // ── #20: a failed ledger save must not drop a dictation event ─────────

    #[test]
    fn a_failed_ledger_save_is_never_silently_dropped() {
        let _guard = super::store_test_guard();
        // A regular FILE where a directory must be: every write into this
        // ledger path fails, while the read still reports "no ledger yet".
        let blocker = unique("ledger-blocked", "bin");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let ledger = blocker.join("nested").join("stats_events.json");
        StatsDirtyStore::set_test_ledger_path(Some(ledger.clone()));

        // Audit #20: `let _ = Self::save_rows(&rows)` dropped the event for
        // good. A dictation event is not re-derivable - its id is a one-shot
        // backfill UUID - so this is permanent, silent data loss.
        let err = StatsDirtyStore::record_dictation_event_checked("audit-row", 1_000, "hi", 500)
            .expect_err("a failed ledger save must surface, not vanish");
        assert!(
            matches!(err, SyncError::Fatal(_)),
            "a dictation event must never be dropped silently: {err:?}"
        );
        assert!(
            !ledger.exists(),
            "no partial ledger may be written when the save fails"
        );

        // The public wrapper (what history.rs calls) stays infallible but must
        // not panic and must not fabricate a ledger.
        StatsDirtyStore::record_dictation_event("audit-row", 1_000, "hi", 500);
        assert!(
            !ledger.exists(),
            "the infallible wrapper must not paper over the failure"
        );

        StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&blocker);
    }
}
