// Fluence Windows - Custom Dictionary module
// Stores spoken→corrected word/phrase pairs in a JSON file.
// Applied as post-processing after every transcription.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use unicode_normalization::UnicodeNormalization;

fn default_entry_kind() -> String {
    KIND_CORRECTION.to_string()
}

/// Entry kinds. A `correction` entry contributes words to the STT vocabulary
/// prompt; an `expansion` entry is excluded from it entirely (see
/// `transcribe::build_vocabulary_hint`). Both are **local-only annotations**:
/// the frozen v1.2 wire format carries no `kind` field (F1), so every peer
/// reconstitutes it as `correction`.
pub const KIND_CORRECTION: &str = "correction";
pub const KIND_EXPANSION: &str = "expansion";

/// Normalize a caller-supplied kind to a supported value.
///
/// Returns `None` for an unsupported value so write paths can reject it
/// explicitly instead of persisting a value that cannot round-trip. This
/// matters because the sync payload is frozen and always decodes as
/// `correction`: an unsupported local value that reached the payload made the
/// frozen round-trip gate in `sync::frozen::sync_domain` fail with
/// `SyncError::Fatal`, which latches the scheduler and stops ALL domains from
/// syncing until an unrelated local edit or a restart (audit finding C1).
pub fn normalize_kind(raw: Option<&str>) -> Option<&'static str> {
    match raw.map(str::trim) {
        None => Some(KIND_CORRECTION),
        Some(k) if k.eq_ignore_ascii_case(KIND_CORRECTION) => Some(KIND_CORRECTION),
        Some(k) if k.eq_ignore_ascii_case(KIND_EXPANSION) => Some(KIND_EXPANSION),
        Some(_) => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub id: String,
    pub spoken: String,
    pub corrected: String,
    /// Entry type: "correction" (word/phrase fix) or "expansion"
    #[serde(default = "default_entry_kind")]
    pub kind: String,
    // Frozen v1.1 sync metadata
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub deleted_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default = "default_true")]
    pub is_enabled: bool,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub ever_pushed: bool,
    #[serde(default)]
    pub sync_account: Option<String>,
    // Legacy fields for migration (kept for load compatibility, ignored after migration)
    #[serde(default)]
    pub sync_state: Option<String>,
    #[serde(default)]
    pub server_file_id: Option<String>,
    #[serde(default)]
    pub quarantine_reason: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Bounded validation limits (security hardening, additive only).
/// Normal dictionary use is far below these; oversized input is rejected
/// with a clean error instead of causing OOM or megabyte regex compiles.
pub const MAX_IMPORT_JSON_LEN: usize = 1_000_000;
pub const MAX_SPOKEN_CHARS: usize = 200;
pub const MAX_CORRECTED_CHARS: usize = 1_000;

impl Default for DictionaryEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            spoken: String::new(),
            corrected: String::new(),
            kind: default_entry_kind(),
            created_at: None,
            deleted_at: None,
            updated_at: None,
            device_id: None,
            is_enabled: true,
            dirty: false,
            ever_pushed: false,
            sync_account: None,
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub entry: DictionaryEntry,
    pub regex: regex::Regex,
}

static DICTIONARY_CACHE: Mutex<Option<Vec<CachedEntry>>> = Mutex::new(None);

/// Uses the shared `sync::stores::base_data_dir()` so test builds can never
/// write the real dictionary. Unset in production, so the path is unchanged.
fn dictionary_path() -> PathBuf {
    let mut path = crate::sync::stores::base_data_dir();
    path.push("Fluence");
    path.push("dictionary.json");
    path
}

pub(crate) fn load_dictionary_internal() -> Result<Vec<DictionaryEntry>> {
    load_dictionary_file(&dictionary_path())
}

/// Path-parameterized core of [`load_dictionary_internal`]. Tests drive this
/// with unique temp paths so corrupt-file recovery is verified hermetically,
/// never against the shared test data root.
pub(crate) fn load_dictionary_file(path: &std::path::Path) -> Result<Vec<DictionaryEntry>> {
    match crate::sync::recover::read_json_file(path) {
        // Missing file is a fresh store, not corruption: no backup, writes
        // proceed normally.
        Ok(None) => Ok(Vec::new()),
        Ok(Some(entries)) => Ok(entries),
        // Corrupt bytes are preserved aside inside `read_json_file`; surface
        // the error so write paths fail closed instead of persisting an
        // empty store over unreadable state (Phase E).
        Err(e) => Err(anyhow::anyhow!("unreadable dictionary store: {e:?}")),
    }
}

pub(crate) fn save_dictionary_internal(entries: &[DictionaryEntry]) -> Result<()> {
    let path = dictionary_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(entries)?;
    // Atomic write: tmp + flush + rename.
    //
    // Durability, stated precisely: the tmp's CONTENTS are flushed before the
    // rename, so a power loss can never expose a half-written store under the
    // real name. The rename's own directory entry is NOT flushed — that needs a
    // parent-directory flush, which none of these writers do. On power loss the
    // destination may therefore still hold the PREVIOUS good copy rather than
    // the new one. That is the safe direction (no truncation, no corruption),
    // but it is not "durable" in the strict sense and should not be described as
    // such.
    //
    // The tmp name MUST be unique per caller. This used to be the fixed
    // `path.with_extension("json.tmp")`, the exact bug `settings.rs:371-379`
    // documents and `sync/stores.rs` + `account_scope.rs` already fixed: two
    // concurrent writers both wrote the SAME tmp, then both renamed it, so the
    // loser either renamed a tmp the winner had already consumed (os error 2) or
    // renamed a partially-written tmp onto the destination — truncating a live
    // store. Audit #22 fixed the two files it happened to touch and left these
    // two. pid + monotonic sequence, in the destination's own directory so the
    // rename stays same-volume atomic, exactly as settings.rs does it.
    let tmp_path = {
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".tmp.{}.{}", std::process::id(), seq));
        PathBuf::from(name)
    };
    let write_result = (|| -> Result<()> {
        fs::write(&tmp_path, &data)?;
        // WRITE handle required: on Windows `sync_all` -> `FlushFileBuffers` needs
        // `GENERIC_WRITE`, so the previous `File::open` (O_RDONLY) made this a no-op
        // despite the durability intent. Best effort, non-fatal.
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&tmp_path) {
            let _ = f.sync_all();
        }
        fs::rename(&tmp_path, &path)?;
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.sync_all();
        }
        Ok(())
    })();
    if write_result.is_err() {
        // Never leave a stray tmp behind on the failure path. Its name is
        // unpredictable, so nothing else can clean it up.
        let _ = fs::remove_file(&tmp_path);
    }
    write_result?;
    // Sync imports write through this function too. Drop compiled rules so
    // the next transcription uses the merged file immediately.
    invalidate_cache();
    Ok(())
}

fn cache_entries(entries: Vec<DictionaryEntry>) -> Vec<CachedEntry> {
    let mut cached = Vec::with_capacity(entries.len());
    for entry in entries {
        // Skip oversized legacy rows so a corrupt file cannot force a
        // megabyte regex compile; new writes are already capped above.
        if entry.spoken.chars().count() > MAX_SPOKEN_CHARS {
            log::warn!(
                "Skipping dictionary entry with oversized spoken text ({} chars)",
                entry.spoken.chars().count()
            );
            continue;
        }
        // Case-insensitive whole-word replacement pattern
        let pattern = format!("(?i)\\b{}\\b", regex_escape(&entry.spoken));
        match regex::Regex::new(&pattern) {
            Ok(re) => {
                cached.push(CachedEntry { entry, regex: re });
            }
            Err(e) => {
                log::warn!(
                    "Invalid regex pattern for spoken phrase ({} chars): {}",
                    entry.spoken.chars().count(),
                    e
                );
            }
        }
    }
    cached
}

/// Apply dictionary corrections to transcribed text (case-insensitive word boundary matching)
pub fn apply_corrections(text: &str) -> String {
    let cache = DICTIONARY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let entries = match cache.as_ref() {
        Some(e) => e.clone(),
        None => {
            drop(cache);
            let active_account = crate::sync::metadata::current_account_hash();
            let loaded: Vec<DictionaryEntry> = load_dictionary_internal()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.deleted_at.is_none() && e.is_enabled) // deleted never apply, disabled never apply
                .filter(|e| {
                    crate::sync::metadata::belongs_to_account(
                        e.sync_account.as_deref(),
                        active_account.as_deref(),
                    )
                })
                .collect();
            let cached = cache_entries(loaded);
            let mut cache2 = DICTIONARY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            *cache2 = Some(cached.clone());
            cached
        }
    };

    let mut result = text.to_string();
    for cached_entry in &entries {
        result = cached_entry
            .regex
            .replace_all(&result, cached_entry.entry.corrected.as_str())
            .to_string();
    }
    result
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "[](){}*+?^$|.\\.".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

pub(crate) fn invalidate_cache() {
    let mut cache = DICTIONARY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache = None;
}

/// Canonical, case-insensitive key for a spoken→corrected pair.
///
/// This is the single normalization path used when comparing
/// dictionary entries across modules (Dictionary, Auto Learn,
/// Suggestions). Comparisons must go through this function instead
/// of introducing ad-hoc `to_lowercase()` checks.
pub fn canonical_entry_key(spoken: &str, corrected: &str) -> String {
    format!(
        "{}\u{0}{}",
        spoken.trim().to_lowercase(),
        corrected.trim().to_lowercase()
    )
}

/// Normalize spoken/corrected text: trim whitespace and reject blanks.
fn normalize_entry_text(spoken: &str, corrected: &str) -> Result<(String, String), String> {
    let spoken = spoken.trim().to_string();
    let corrected = corrected.trim().to_string();
    if spoken.is_empty() || corrected.is_empty() {
        return Err("Spoken and corrected text must not be empty".to_string());
    }
    if spoken.chars().count() > MAX_SPOKEN_CHARS {
        return Err(format!(
            "Spoken text too long ({} chars). Maximum is {} characters.",
            spoken.chars().count(),
            MAX_SPOKEN_CHARS
        ));
    }
    if corrected.chars().count() > MAX_CORRECTED_CHARS {
        return Err(format!(
            "Corrected text too long ({} chars). Maximum is {} characters.",
            corrected.chars().count(),
            MAX_CORRECTED_CHARS
        ));
    }
    Ok((spoken, corrected))
}

/// True when an exact spoken→corrected pair (case-insensitive, trimmed)
/// already exists in the list.
fn entries_already_have(entries: &[DictionaryEntry], spoken: &str, corrected: &str) -> bool {
    let key = canonical_entry_key(spoken, corrected);
    entries
        .iter()
        .any(|e| canonical_entry_key(&e.spoken, &e.corrected) == key)
}

/// Merge incoming entries into an existing list. Blank entries and exact
/// duplicates (same spoken→corrected pair) are skipped; incoming entries
/// get fresh ids and a creation timestamp. Returns the merged list, the
/// number actually added, and how many entries had an unsupported `kind`
/// coerced to [`KIND_CORRECTION`] (surfaced to the user - those entries can
/// never round-trip through the frozen sync wire, so silently keeping the
/// value would be a lie).
fn merge_dictionary_entries(
    existing: &[DictionaryEntry],
    incoming: Vec<DictionaryEntry>,
) -> (Vec<DictionaryEntry>, usize, usize) {
    let mut entries = existing.to_vec();
    let mut added = 0;
    let mut coerced_kind = 0;
    let now = chrono::Utc::now().timestamp_millis();
    for mut entry in incoming {
        entry.spoken = entry.spoken.trim().to_string();
        entry.corrected = entry.corrected.trim().to_string();
        if entry.spoken.is_empty() || entry.corrected.is_empty() {
            continue;
        }
        // Imports are bulk data: an unsupported `kind` is coerced to
        // `correction`, logged, AND counted so the caller can tell the user.
        // A preserved unknown value could never round-trip through the frozen
        // sync payload and would wedge the scheduler via the round-trip gate
        // (audit finding C1).
        entry.kind = match normalize_kind(Some(entry.kind.as_str())) {
            Some(k) => k.to_string(),
            None => {
                coerced_kind += 1;
                log::warn!(
                    "dictionary import: unsupported entry kind '{}' coerced to '{KIND_CORRECTION}'",
                    entry.kind
                );
                KIND_CORRECTION.to_string()
            }
        };
        if entry.spoken.chars().count() > MAX_SPOKEN_CHARS
            || entry.corrected.chars().count() > MAX_CORRECTED_CHARS
        {
            continue;
        }
        if entries_already_have(&entries, &entry.spoken, &entry.corrected) {
            continue;
        }
        entry.id = uuid::Uuid::new_v4().to_string();
        entry.created_at = Some(now);
        entries.push(entry);
        added += 1;
    }
    (entries, added, coerced_kind)
}

/// Live entries only (not tombstoned) - the user-facing view (§30.2).
fn live_entries(entries: Vec<DictionaryEntry>) -> Vec<DictionaryEntry> {
    entries
        .into_iter()
        .filter(|e| e.deleted_at.is_none())
        .collect()
}

// Tauri Commands

#[tauri::command]
pub fn get_dictionary() -> Result<Vec<DictionaryEntry>, String> {
    let active = crate::sync::metadata::current_account_hash();
    Ok(live_entries(
        load_dictionary_internal()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|e| {
                crate::sync::metadata::belongs_to_account(
                    e.sync_account.as_deref(),
                    active.as_deref(),
                )
            })
            .collect(),
    ))
}

/// Scheduler-free core of [`add_dictionary_entry`]: identical validation,
/// persistence and cache invalidation, but no sync wake-up signal. Used by
/// the auto-accept path, which runs on threads without Tauri state (notably
/// the post-injection monitor thread); the periodic sync pass picks up the
/// dirty row. Callers needing an immediate sync wake-up must send
/// `SyncCommand::LocalChange` themselves.
pub(crate) fn add_dictionary_entry_internal(
    spoken: String,
    corrected: String,
    kind: Option<String>,
) -> Result<DictionaryEntry, String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let (spoken, corrected) = normalize_entry_text(&spoken, &corrected)?;
    let mut all_entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    let active_account = crate::sync::metadata::current_account_hash();
    // Frozen v1.1: businessKey = lower(trim(spoken)) must be unique among live+disabled (non-deleted)
    let bk = spoken.trim().to_lowercase();
    if all_entries.iter().any(|e| {
        e.deleted_at.is_none()
            && crate::sync::metadata::belongs_to_account(
                e.sync_account.as_deref(),
                active_account.as_deref(),
            )
            && e.spoken.trim().to_lowercase() == bk
    }) {
        return Err(format!("Dictionary entry '{}' already exists", spoken));
    }
    let mut meta = crate::sync::metadata::SyncMetadata::load();
    let device_id = meta.ensure_device_id();
    // Monotonic per-account maxSeen (or global if no account yet) - prevents clock skew from making stale win
    let account_hash = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key)
        .map(|e| crate::sync::metadata::account_hash_from_email(&e));
    let max_seen = account_hash
        .as_deref()
        .and_then(|h| meta.for_account(h).map(|s| s.max_seen))
        .unwrap_or(0);
    let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
    if let Some(h) = account_hash.as_deref() {
        meta.update_max_seen(h, new_max);
    }
    let entry = DictionaryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        spoken,
        corrected,
        kind: match normalize_kind(kind.as_deref()) {
            Some(k) => k.to_string(),
            None => {
                return Err(format!(
                    "Unsupported dictionary entry kind (expected '{KIND_CORRECTION}' or '{KIND_EXPANSION}')"
                ));
            }
        },
        created_at: Some(now),
        deleted_at: None,
        updated_at: Some(now),
        device_id: Some(device_id),
        is_enabled: true,
        dirty: true,
        ever_pushed: false,
        sync_account: account_hash,
        sync_state: None,
        server_file_id: None,
        quarantine_reason: None,
    };
    all_entries.push(entry.clone());
    save_dictionary_internal(&all_entries).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(entry)
}

#[tauri::command]
pub fn add_dictionary_entry(
    window: tauri::Window,
    spoken: String,
    corrected: String,
    kind: Option<String>,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<DictionaryEntry, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let entry = add_dictionary_entry_internal(spoken, corrected, kind)?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    Ok(entry)
}

#[tauri::command]
pub fn update_dictionary_entry(
    window: tauri::Window,
    id: String,
    spoken: String,
    corrected: String,
    kind: Option<String>,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    update_dictionary_entry_internal(id, spoken, corrected, kind)?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(())
}

/// Outcome of [`update_dictionary_entry_internal`]: whether the edit kept the
/// record's sync identity or moved its business key (H3 rename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DictionaryUpdateOutcome {
    /// Same business key: the row was edited in place, sync identity kept.
    Edited,
    /// Business key changed: the old row was retired and a new row with a
    /// fresh id — hence a fresh wire `sync_id` (`stores.rs` maps `sync_id`
    /// from the row id) — carries the renamed entry. `old_tombstoned` is
    /// true when the old row had been pushed (a durable tombstone propagates
    /// the deletion) and false when it had never been pushed (hard-deleted;
    /// peers never saw it, so there is nothing to propagate).
    Renamed {
        new_id: String,
        old_tombstoned: bool,
    },
}

/// Sync business key of a spoken form — the exact identity
/// [`crate::sync::domain::DictionaryItem::business_key`] derives (NFC +
/// lowercase + trim), so rename detection agrees with what the merge treats
/// as a key move.
pub(crate) fn dict_sync_business_key(spoken: &str) -> String {
    spoken.trim().nfc().collect::<String>().to_lowercase()
}

/// Scheduler-free core of [`update_dictionary_entry`]: identical validation,
/// lookup, collision, stamping and persistence, but no ACL gate (the command
/// owns it) and no sync wake-up signal (the caller sends one
/// `LocalChange` covering both rows of a rename).
///
/// H3 rename rule: when the spoken text moves the sync business key, the old
/// row is NOT edited in place. It is retired — tombstoned when it had been
/// pushed (the tombstone rides the normal per-key LWW under the OLD key, so
/// offline peers adopt the deletion and can never resurrect the entry), or
/// hard-deleted when it had never been pushed — and a new row with a fresh
/// id carries the renamed entry. Both mutations share one clock stamp and
/// one atomic file save. A same-key edit (including case-only changes, whose
/// business key is unchanged) keeps the in-place path with identical
/// behavior to before.
pub(crate) fn update_dictionary_entry_internal(
    id: String,
    spoken: String,
    corrected: String,
    kind: Option<String>,
) -> Result<DictionaryUpdateOutcome, String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let (spoken, corrected) = normalize_entry_text(&spoken, &corrected)?;
    let mut all_entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    let active_account = crate::sync::metadata::current_account_hash();
    let bk = spoken.trim().to_lowercase();
    if all_entries.iter().any(|other| {
        other.id != id
            && other.deleted_at.is_none()
            && crate::sync::metadata::belongs_to_account(
                other.sync_account.as_deref(),
                active_account.as_deref(),
            )
            && other.spoken.trim().to_lowercase() == bk
    }) {
        return Err(format!("Dictionary entry '{}' already exists", spoken));
    }
    // Frozen v1.1: same syncId on edit, update updatedAt/deviceId/dirty in same TX
    let mut meta = crate::sync::metadata::SyncMetadata::load();
    let device_id = meta.ensure_device_id();
    let account_hash = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key)
        .map(|e| crate::sync::metadata::account_hash_from_email(&e));
    let max_seen = account_hash
        .as_deref()
        .and_then(|h| meta.for_account(h).map(|s| s.max_seen))
        .unwrap_or(0);
    let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
    if let Some(h) = account_hash.clone() {
        meta.update_max_seen(&h, new_max);
    }
    // We need account hash for maxSeen? For local edit without account, use device's maxSeen global
    // For simplicity, update global maxSeen via metadata (per-account will be updated on sync)
    let pos = all_entries
        .iter()
        .position(|e| e.id == id)
        .ok_or_else(|| "Dictionary entry not found".to_string())?;
    if !crate::sync::metadata::belongs_to_account(
        all_entries[pos].sync_account.as_deref(),
        active_account.as_deref(),
    ) {
        return Err("Dictionary entry belongs to another account".to_string());
    }
    if all_entries[pos].deleted_at.is_some() {
        return Err("Cannot edit a deleted entry".to_string());
    }
    let resolved_kind = match kind.as_deref() {
        None => None,
        Some(k) => Some(
            normalize_kind(Some(k))
                .ok_or_else(|| {
                    format!(
                        "Unsupported dictionary entry kind (expected '{KIND_CORRECTION}' or '{KIND_EXPANSION}')"
                    )
                })?
                .to_string(),
        ),
    };
    let old_key = dict_sync_business_key(&all_entries[pos].spoken);
    let new_key = dict_sync_business_key(&spoken);
    let dismissed_pair: Option<(String, String)>;
    let outcome = if old_key == new_key {
        let entry = &mut all_entries[pos];
        entry.spoken = spoken.clone();
        entry.corrected = corrected.clone();
        if let Some(k) = resolved_kind {
            entry.kind = k;
        }
        entry.updated_at = Some(now);
        entry.device_id = Some(device_id.clone());
        entry.dirty = true;
        // ever_pushed stays as is
        dismissed_pair = None;
        DictionaryUpdateOutcome::Edited
    } else {
        let old = all_entries[pos].clone();
        let old_tombstoned = old.ever_pushed;
        if old_tombstoned {
            // Durable tombstone under the OLD key: keeps the old spoken
            // text (so the business key recomputes to the old key), the
            // old id (so the wire sync_id is unchanged), and the account
            // partition. Peers adopt it by plain per-key LWW.
            let slot = &mut all_entries[pos];
            slot.deleted_at = Some(now);
            slot.updated_at = Some(now);
            slot.device_id = Some(device_id.clone());
            slot.dirty = true;
            // isEnabled remains but deletedAt takes precedence (tombstoneBit=1)
        } else {
            // Never pushed: peers never saw this row, so there is no
            // deletion to propagate — remove it locally.
            all_entries.remove(pos);
        }
        let new_id = uuid::Uuid::new_v4().to_string();
        all_entries.push(DictionaryEntry {
            id: new_id.clone(),
            spoken: spoken.clone(),
            corrected: corrected.clone(),
            kind: resolved_kind.unwrap_or(old.kind.clone()),
            created_at: Some(now),
            deleted_at: None,
            updated_at: Some(now),
            device_id: Some(device_id.clone()),
            is_enabled: old.is_enabled,
            dirty: true,
            ever_pushed: false,
            sync_account: old.sync_account.clone(),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });
        dismissed_pair = Some((old.spoken.clone(), old.corrected.clone()));
        DictionaryUpdateOutcome::Renamed {
            new_id,
            old_tombstoned,
        }
    };
    // Update maxSeen persisted (use a dummy account hash for local clock)
    // We'll store maxSeen under a special key "__local__" or just update device's global
    // For now, we update per-account if we have account, else just don't persist maxSeen here (sync will handle)
    save_dictionary_internal(&all_entries).map_err(|e| e.to_string())?;
    // Delete→dismiss linkage (rename retires the old identity too): the io
    // lock is released first - dismiss takes SUGGESTION_LOCK, the reverse
    // order of manual accept, so holding both would risk inversion.
    // Suggestion failures never fail the update.
    drop(_io);
    if let Some((spoken, corrected)) = dismissed_pair {
        if let Err(e) = crate::suggestion::dismiss_matching_suggestions(&spoken, &corrected) {
            log::warn!(
                "Failed to dismiss suggestions for renamed dictionary entry: {}",
                e
            );
        }
    }
    invalidate_cache();
    Ok(outcome)
}

/// Scheduler-free core of [`delete_dictionary_entry`]: identical lookup,
/// tombstone/hard-delete, persistence, delete→dismiss linkage and cache
/// invalidation, but no sync wake-up signal (the periodic sync pass picks
/// up the change). Used by the revert-auto-accept path.
pub(crate) fn delete_dictionary_entry_internal(id: String) -> Result<(), String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    let mut meta = crate::sync::metadata::SyncMetadata::load();
    let device_id = meta.ensure_device_id();
    let account_hash = crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key)
        .map(|e| crate::sync::metadata::account_hash_from_email(&e));
    let max_seen = account_hash
        .as_deref()
        .and_then(|h| meta.for_account(h).map(|s| s.max_seen))
        .unwrap_or(0);
    let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
    if let Some(h) = account_hash {
        meta.update_max_seen(&h, new_max);
    }
    let active_account = crate::sync::metadata::current_account_hash();
    let mut to_hard_delete = false;
    let mut deleted_pair: Option<(String, String)> = None;
    for entry in entries.iter_mut() {
        if entry.id == id {
            deleted_pair = Some((entry.spoken.clone(), entry.corrected.clone()));
            if !crate::sync::metadata::belongs_to_account(
                entry.sync_account.as_deref(),
                active_account.as_deref(),
            ) {
                return Err("Dictionary entry belongs to another account".to_string());
            }
            if !entry.ever_pushed {
                // Never pushed → hard delete (everPushed distinguishes)
                to_hard_delete = true;
            } else {
                // Tombstone forever never GC
                entry.deleted_at = Some(now);
                entry.updated_at = Some(now);
                entry.device_id = Some(device_id.clone());
                entry.dirty = true;
                // isEnabled remains but deletedAt takes precedence (tombstoneBit=1)
            }
        }
    }
    if to_hard_delete {
        entries.retain(|e| e.id != id);
    }
    save_dictionary_internal(&entries).map_err(|e| e.to_string())?;
    // Delete→dismiss linkage: the io lock is released first - dismiss takes
    // SUGGESTION_LOCK, the reverse order of manual accept, so holding both
    // would risk inversion. Suggestion failures never fail the delete.
    drop(_io);
    if let Some((spoken, corrected)) = deleted_pair {
        if let Err(e) = crate::suggestion::dismiss_matching_suggestions(&spoken, &corrected) {
            log::warn!(
                "Failed to dismiss suggestions for deleted dictionary entry: {}",
                e
            );
        }
    }
    invalidate_cache();
    Ok(())
}

/// Delete one dictionary entry (main window only). Wraps
/// [`delete_dictionary_entry_internal`] and adds the ACL gate plus the sync
/// wake-up the internal path deliberately omits.
#[tauri::command]
pub fn delete_dictionary_entry(
    window: tauri::Window,
    id: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    delete_dictionary_entry_internal(id)?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    Ok(())
}

/// Result of a dictionary import. `coerced_kind` counts entries whose `kind`
/// was an unsupported value and was coerced to [`KIND_CORRECTION`] - those
/// entries would otherwise be unable to sync (they can never round-trip
/// through the frozen v1.2 wire format), so the UI must say so instead of
/// silently importing them as corrections.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DictionaryImportResult {
    pub added: usize,
    pub coerced_kind: usize,
}

/// Shared import core behind both import commands: identical behaviour, the
/// only difference is how much of the outcome is reported to the caller.
fn import_dictionary_impl(
    window: &tauri::Window,
    json_data: String,
    scheduler: &tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<DictionaryImportResult, String> {
    crate::acl::require_caller(window, &[crate::acl::MAIN_WINDOW])?;
    let _io = crate::sync::io_lock::io_lock_guard();
    if json_data.len() > MAX_IMPORT_JSON_LEN {
        return Err(format!(
            "Dictionary import too large ({} bytes). Maximum is {} bytes (~1 MB).",
            json_data.len(),
            MAX_IMPORT_JSON_LEN
        ));
    }
    let mut new_entries: Vec<DictionaryEntry> =
        serde_json::from_str(&json_data).map_err(|e| e.to_string())?;
    // Stamp imports to the active account so they don't become global `None` rows visible to any account.
    let active_account = crate::sync::metadata::current_account_hash();
    if let Some(hash) = active_account.clone() {
        let mut meta = crate::sync::metadata::SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        let max_seen = meta.for_account(&hash).map(|s| s.max_seen).unwrap_or(0);
        let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
        meta.update_max_seen(&hash, new_max);
        for e in new_entries.iter_mut() {
            if e.sync_account.is_none() {
                e.sync_account = Some(hash.clone());
                e.updated_at = Some(now);
                e.device_id = Some(device_id.clone());
                e.dirty = true;
                e.ever_pushed = false;
            }
        }
    }
    let existing = load_dictionary_internal().map_err(|e| e.to_string())?;
    let (merged, added, coerced_kind) = merge_dictionary_entries(&existing, new_entries);
    save_dictionary_internal(&merged).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(DictionaryImportResult {
        added,
        coerced_kind,
    })
}

/// Legacy import command: byte-identical contract to the pre-remediation
/// build (returns the added count as a bare number). Kept so any out-of-tree
/// caller invoking `import_dictionary` keeps working; in-repo UIs call
/// [`import_dictionary_report`] instead.
#[tauri::command]
pub fn import_dictionary(
    window: tauri::Window,
    json_data: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<usize, String> {
    Ok(import_dictionary_impl(&window, json_data, &scheduler)?.added)
}

/// Rich import outcome: identical import to [`import_dictionary`], but also
/// reporting how many entries had an unrecognized `kind` coerced to
/// `correction` - those entries cannot sync, so the UI must say so instead of
/// silently importing them as corrections.
#[tauri::command]
pub fn import_dictionary_report(
    window: tauri::Window,
    json_data: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<DictionaryImportResult, String> {
    import_dictionary_impl(&window, json_data, &scheduler)
}

/// Read a user-picked dictionary import file server-side (main window only).
/// The open-file dialog is the authorization; this only bounds size (same
/// 1 MB cap as `import_dictionary`) and requires a regular UTF-8 file.
/// Lets the renderer drop direct `fs` plugin access entirely.
#[tauri::command]
pub fn read_import_file(window: tauri::Window, path: String) -> Result<String, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("Cannot read file: {e}"))?;
    if !meta.is_file() {
        return Err("Selected path is not a file".to_string());
    }
    if meta.len() > MAX_IMPORT_JSON_LEN as u64 {
        return Err(format!(
            "File too large ({} bytes). Maximum is {} bytes (~1 MB).",
            meta.len(),
            MAX_IMPORT_JSON_LEN
        ));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("Cannot read file: {e}"))
}

#[tauri::command]
pub fn export_dictionary() -> Result<String, String> {
    let active = crate::sync::metadata::current_account_hash();
    let entries: Vec<DictionaryEntry> = load_dictionary_internal()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|e| {
            crate::sync::metadata::belongs_to_account(e.sync_account.as_deref(), active.as_deref())
        })
        .collect();
    serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Sync note (frozen v1.2): the sync-facing store lives in
// `crate::sync::stores::DictionaryDirtyStore`. History never syncs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- kind validation (audit finding C1) ------------------------------

    #[test]
    fn normalize_kind_accepts_supported_values() {
        assert_eq!(normalize_kind(None), Some(KIND_CORRECTION));
        assert_eq!(normalize_kind(Some("correction")), Some(KIND_CORRECTION));
        assert_eq!(normalize_kind(Some("expansion")), Some(KIND_EXPANSION));
        assert_eq!(
            normalize_kind(Some("  Expansion  ")),
            Some(KIND_EXPANSION),
            "surrounding whitespace and casing are tolerated"
        );
    }

    #[test]
    fn normalize_kind_rejects_unsupported_values() {
        // Anything else must never be persisted verbatim: it cannot round-trip
        // through the frozen wire payload and would fail the round-trip gate.
        // (Surrounding whitespace and casing are tolerated, so "Expansion " is
        // still the supported `expansion`.)
        for bad in ["exp", "corrections", "", "  ", "fix", "expansions"] {
            assert_eq!(
                normalize_kind(Some(bad)),
                None,
                "'{bad}' must not be accepted as a kind"
            );
        }
    }

    #[test]
    fn import_coerces_unsupported_kind_and_keeps_valid_expansion() {
        let mut good = entry("a", "hello", "hello");
        good.kind = KIND_EXPANSION.to_string();
        let mut bad = entry("b", "world", "world!");
        bad.kind = "definitely-not-a-kind".to_string();

        let (merged, added, coerced) = merge_dictionary_entries(&[], vec![good, bad]);
        assert_eq!(added, 2);
        assert_eq!(
            coerced, 1,
            "the coerced entry must be counted so the UI can surface it"
        );
        assert_eq!(merged[0].kind, KIND_EXPANSION, "valid kind is preserved");
        assert_eq!(
            merged[1].kind, KIND_CORRECTION,
            "unsupported kind is coerced so it can never wedge the sync round-trip gate"
        );
    }

    #[test]
    fn test_canonical_entry_key_case_insensitive() {
        assert_eq!(
            canonical_entry_key("Shunade", "SINEAD"),
            canonical_entry_key("shunade", "sinead")
        );
    }

    #[test]
    fn test_canonical_entry_key_trims_whitespace() {
        assert_eq!(
            canonical_entry_key("  shunade ", " Sinead "),
            canonical_entry_key("shunade", "Sinead")
        );
    }

    #[test]
    fn test_canonical_entry_key_pairs_do_not_collide() {
        assert_ne!(
            canonical_entry_key("a b", "c"),
            canonical_entry_key("a", "b c")
        );
    }

    fn entry(id: &str, spoken: &str, corrected: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: id.to_string(),
            spoken: spoken.to_string(),
            corrected: corrected.to_string(),
            kind: "correction".to_string(),
            created_at: Some(1713456000123),
            deleted_at: None,
            updated_at: Some(1713456000123),
            device_id: Some("00000000-0000-4000-8000-000000000001".to_string()),
            is_enabled: true,
            dirty: false,
            ever_pushed: false,
            sync_state: None,
            server_file_id: None,
            sync_account: None,
            quarantine_reason: None,
        }
    }

    #[test]
    fn test_normalize_entry_text_trims_and_rejects_blanks() {
        assert_eq!(
            normalize_entry_text("  tori ", "  Tauri ").unwrap(),
            ("tori".to_string(), "Tauri".to_string())
        );
        assert!(normalize_entry_text("   ", "Tauri").is_err());
        assert!(normalize_entry_text("tori", "  ").is_err());
    }

    #[test]
    fn test_entries_already_have_is_case_insensitive() {
        let entries = vec![entry("1", "shunade", "Sinead")];
        assert!(entries_already_have(&entries, "SHUNADE", "sinead"));
        assert!(entries_already_have(&entries, "  shunade ", "Sinead "));
        assert!(!entries_already_have(&entries, "shunade", "Shane"));
    }

    #[test]
    fn test_merge_dictionary_entries_skips_duplicates_and_blanks() {
        let existing = vec![entry("1", "tori", "Tauri")];
        let incoming = vec![
            entry("x", "github", "GitHub"),
            entry("y", "TORI", "tauri"),
            entry("z", "  ", "APK"),
            entry("w", "apike", ""),
            entry("v", "grok", "Groq"),
        ];
        let (merged, added, coerced) = merge_dictionary_entries(&existing, incoming);
        assert_eq!(added, 2);
        assert_eq!(coerced, 0, "valid kinds are not coerced");
        assert_eq!(merged.len(), 3);
        assert!(merged.iter().any(|e| e.spoken == "github"));
        assert!(merged.iter().any(|e| e.spoken == "grok"));
        assert!(merged.iter().all(|e| e.id != "x" && e.id != "v"));
        assert_eq!(merged[0].id, "1", "existing entries keep their ids");
        assert!(
            merged.iter().all(|e| e.created_at.is_some()),
            "merged entries always carry a creation timestamp"
        );
    }

    // Edit semantics --------------------------------------------------------

    #[test]
    fn test_update_tombstones_old_and_creates_new_uuid() {
        let uploaded = entry("1", "tori", "Tauri");
        let mut uploaded = uploaded;
        let mut entries = vec![uploaded];

        let now = chrono::Utc::now().timestamp_millis();
        if let Some(old) = entries.iter_mut().find(|e| e.id == "1") {
            old.deleted_at = Some(now);
            old.dirty = true;
        }
        entries.push(DictionaryEntry {
            id: "new-uuid".to_string(),
            spoken: "tori".to_string(),
            corrected: "Tauri 2".to_string(),
            kind: "correction".to_string(),
            created_at: Some(now),
            deleted_at: None,
            updated_at: Some(now),
            device_id: Some("00000000-0000-4000-8000-000000000001".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: None,
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        });

        assert!(entries[0].deleted_at.is_some());
        assert_ne!(entries[0].id, entries[1].id);
        assert_eq!(entries[1].corrected, "Tauri 2");
    }

    #[test]
    fn test_delete_semantics_uploaded_vs_never_uploaded() {
        // Never uploaded → hard delete.
        let never = entry("1", "tori", "Tauri");
        assert!(never.server_file_id.is_none());
        let mut entries = vec![never.clone()];
        entries.retain(|e| e.id != "1");
        assert!(entries.is_empty());

        // Uploaded → tombstone, never removed.
        let uploaded = entry("2", "shunade", "Sinead");
        let mut uploaded = uploaded;
        uploaded.server_file_id = Some("F2".to_string());
        let mut entries = vec![uploaded];
        if let Some(e) = entries.iter_mut().find(|e| e.id == "2") {
            e.deleted_at = Some(1713462000456);
        }
        assert_eq!(entries.len(), 1);
        assert!(entries[0].deleted_at.is_some());
    }

    #[test]
    fn test_get_dictionary_excludes_deleted() {
        let live = entry("1", "tori", "Tauri");
        let deleted = entry("2", "shunade", "Sinead");
        let mut deleted = deleted;
        deleted.deleted_at = Some(1713462000456);
        let shown = live_entries(vec![live.clone(), deleted]);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].id, "1");
    }

    // --- corrupt-file recovery (Phase E) ----------------------------------
    //
    // A malformed dictionary.json must surface an error with the original
    // bytes preserved aside — never silently become an empty store that a
    // later write persists over the evidence.

    fn corrupt_cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("json.corrupt.json"));
        // Numbered siblings, if any.
        for n in 1..100u32 {
            let p = path.with_extension(format!("json.corrupt.{n}.json"));
            if !p.exists() {
                break;
            }
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn corrupt_dictionary_file_errors_and_preserves_bytes() {
        // Hermetic: unique temp path, no shared test data root involved.
        let path = std::env::temp_dir().join(format!(
            "fluence-dict-test-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let garbage = b"{\"entries\": [truncated";
        std::fs::write(&path, garbage).unwrap();

        let loaded = load_dictionary_file(&path);
        assert!(
            loaded.is_err(),
            "corrupt bytes must surface an error, never an empty store: {loaded:?}"
        );
        let backup = path.with_extension("json.corrupt.json");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            garbage,
            "original bytes must be preserved byte-identical"
        );
        assert!(!path.exists(), "corrupt original must be rotated aside");
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn save_merged_refuses_corrupt_file_then_recovers_fresh() {
        use crate::sync::error::SyncError;
        use crate::sync::frozen::DirtyStore;
        use crate::sync::stores::DictionaryDirtyStore;
        let _guard = crate::sync::stores::store_test_guard();
        let path = dictionary_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        corrupt_cleanup(&path);
        let garbage = b"definitely not json";
        std::fs::write(&path, garbage).unwrap();

        // First pass: the load quarantines the bytes and fails closed — no
        // write derived from unreadable state may hit the live path.
        let mut store = DictionaryDirtyStore;
        let winner = crate::sync::domain::DictionaryItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            spoken: "hello".to_string(),
            corrected: "hi".to_string(),
            kind: KIND_CORRECTION.to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 100,
            device_id: "device-a".to_string(),
        };
        let account_hash = format!("{:064x}", 7u128);
        let err = store
            .save_merged(&account_hash, vec![winner.clone()])
            .expect_err("save over a corrupt store must fail closed");
        assert!(
            matches!(err, SyncError::Fatal(_)),
            "corrupt local store is unrecoverable-automatically: {err:?}"
        );
        assert_eq!(
            std::fs::read(path.with_extension("json.corrupt.json")).unwrap(),
            garbage,
            "the failed pass must preserve the evidence, not overwrite it"
        );
        assert!(
            !path.exists(),
            "no fresh store may be written while the corruption is unprocessed"
        );

        // Second pass: quarantine already consumed, live path missing → the
        // store is genuinely fresh and writes proceed normally.
        let mut store = DictionaryDirtyStore;
        store
            .save_merged(&account_hash, vec![winner])
            .expect("fresh store after quarantine must accept writes");
        let after = load_dictionary_internal().expect("fresh store must load");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].spoken, "hello");
        corrupt_cleanup(&path);
        // Remove rows this test wrote (shared test file).
        if path.exists() {
            let remaining: Vec<DictionaryEntry> = load_dictionary_internal()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.sync_account.as_deref() != Some(account_hash.as_str()))
                .collect();
            let _ = save_dictionary_internal(&remaining);
        }
        corrupt_cleanup(&path);
    }

    // --- H3 rename (delete-old + create-new) --------------------------------
    //
    // `update_dictionary_entry_internal` retires the old row and creates a
    // new row with a fresh id (hence a fresh wire sync_id) when the spoken
    // text moves the sync business key. These tests drive the real internal
    // against the guarded test data root with account-scoped cleanup, so
    // parallel tests and the developer's real dictionary are unaffected.
    // Spoken strings use unique gibberish: the rename path also dismisses
    // matching suggestions, and a unique key guarantees that dismissal finds
    // nothing (read-only against the real suggestions file).

    fn h3_entry(id: &str, spoken: &str, ever_pushed: bool) -> DictionaryEntry {
        // sync_account None: `belongs_to_account(None, _)` is always true,
        // so these rows are editable regardless of the machine's real
        // account state. Cleanup filters on the unique spoken prefix.
        DictionaryEntry {
            id: id.to_string(),
            spoken: spoken.to_string(),
            corrected: "H3 Corrected".to_string(),
            kind: KIND_CORRECTION.to_string(),
            created_at: Some(1713456000123),
            deleted_at: None,
            updated_at: Some(1713456000123),
            device_id: Some("00000000-0000-4000-8000-000000000001".to_string()),
            is_enabled: true,
            dirty: false,
            ever_pushed,
            sync_account: None,
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }

    fn h3_cleanup() {
        let remaining: Vec<DictionaryEntry> = load_dictionary_internal()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.spoken.starts_with("h3renx "))
            .collect();
        let _ = save_dictionary_internal(&remaining);
    }

    #[test]
    fn rename_pushed_row_tombstones_old_and_creates_fresh_id() {
        let _guard = crate::sync::stores::store_test_guard();
        h3_cleanup();
        save_dictionary_internal(&[h3_entry("old-1", "h3renx alpha", true)]).unwrap();

        let outcome = update_dictionary_entry_internal(
            "old-1".to_string(),
            "h3renx beta".to_string(),
            "H3 New".to_string(),
            None,
        )
        .expect("rename must succeed");
        let new_id = match outcome {
            DictionaryUpdateOutcome::Renamed {
                new_id,
                old_tombstoned,
            } => {
                assert!(old_tombstoned, "a pushed row must leave a tombstone");
                new_id
            }
            DictionaryUpdateOutcome::Edited => panic!("a key move must not edit in place"),
        };
        assert_ne!(
            new_id, "old-1",
            "the renamed entry needs a fresh sync identity"
        );

        let after = load_dictionary_internal().unwrap();
        let old = after
            .iter()
            .find(|e| e.id == "old-1")
            .expect("tombstone persists");
        assert!(old.deleted_at.is_some(), "old row is a tombstone");
        assert!(old.dirty, "tombstone is dirty so it uploads");
        assert_eq!(old.spoken, "h3renx alpha", "tombstone keeps the old key");
        let new = after
            .iter()
            .find(|e| e.id == new_id)
            .expect("new row exists");
        assert_eq!(new.spoken, "h3renx beta");
        assert!(new.deleted_at.is_none());
        assert!(new.dirty);
        assert!(!new.ever_pushed);
        assert_eq!(new.sync_account, None);
        h3_cleanup();
    }

    #[test]
    fn rename_never_pushed_row_hard_deletes_old() {
        let _guard = crate::sync::stores::store_test_guard();
        h3_cleanup();
        save_dictionary_internal(&[h3_entry("old-2", "h3renx gamma", false)]).unwrap();

        let outcome = update_dictionary_entry_internal(
            "old-2".to_string(),
            "h3renx delta".to_string(),
            "H3 New".to_string(),
            None,
        )
        .expect("rename must succeed");
        match outcome {
            DictionaryUpdateOutcome::Renamed {
                new_id,
                old_tombstoned,
            } => {
                assert!(!old_tombstoned, "a never-pushed row leaves no tombstone");
                assert_ne!(new_id, "old-2");
            }
            DictionaryUpdateOutcome::Edited => panic!("a key move must not edit in place"),
        }

        let after = load_dictionary_internal().unwrap();
        assert!(
            after.iter().all(|e| e.id != "old-2"),
            "the never-pushed old row is removed locally"
        );
        assert!(
            after
                .iter()
                .any(|e| e.spoken == "h3renx delta" && e.deleted_at.is_none()),
            "the renamed entry exists exactly once and live"
        );
        h3_cleanup();
    }

    #[test]
    fn rename_collision_refused_and_old_untouched() {
        let _guard = crate::sync::stores::store_test_guard();
        h3_cleanup();
        save_dictionary_internal(&[
            h3_entry("old-3", "h3renx epsilon", true),
            h3_entry("other-3", "h3renx zeta", true),
        ])
        .unwrap();

        let err = update_dictionary_entry_internal(
            "old-3".to_string(),
            "h3renx zeta".to_string(),
            "H3 New".to_string(),
            None,
        )
        .expect_err("renaming onto a live key must be refused");
        assert!(err.contains("already exists"), "unexpected error: {err}");

        let after = load_dictionary_internal().unwrap();
        let old = after.iter().find(|e| e.id == "old-3").unwrap();
        assert_eq!(
            old.spoken, "h3renx epsilon",
            "refused rename leaves the row byte-identical"
        );
        assert!(old.deleted_at.is_none());
        assert!(!old.dirty);
        h3_cleanup();
    }

    #[test]
    fn edit_same_key_keeps_id_with_no_tombstone() {
        let _guard = crate::sync::stores::store_test_guard();
        h3_cleanup();
        save_dictionary_internal(&[h3_entry("row-4", "h3renx theta", true)]).unwrap();

        let outcome = update_dictionary_entry_internal(
            "row-4".to_string(),
            "h3renx theta".to_string(),
            "H3 Corrected 2".to_string(),
            None,
        )
        .expect("same-key edit must succeed");
        assert_eq!(outcome, DictionaryUpdateOutcome::Edited);

        let after = load_dictionary_internal().unwrap();
        assert_eq!(
            after
                .iter()
                .filter(|e| e.spoken.starts_with("h3renx "))
                .count(),
            1
        );
        let row = after.iter().find(|e| e.id == "row-4").unwrap();
        assert_eq!(row.corrected, "H3 Corrected 2");
        assert!(row.deleted_at.is_none());
        h3_cleanup();
    }

    #[test]
    fn edit_case_only_change_keeps_id() {
        // The sync business key is case-insensitive: a case-only change is
        // NOT a rename and must keep the row (and its sync identity) in place.
        let _guard = crate::sync::stores::store_test_guard();
        h3_cleanup();
        save_dictionary_internal(&[h3_entry("row-5", "h3renx Kappa", true)]).unwrap();

        let outcome = update_dictionary_entry_internal(
            "row-5".to_string(),
            "H3RENX kappa".to_string(),
            "H3 Corrected".to_string(),
            None,
        )
        .expect("case-only edit must succeed");
        assert_eq!(outcome, DictionaryUpdateOutcome::Edited);

        let after = load_dictionary_internal().unwrap();
        assert_eq!(
            after
                .iter()
                .filter(|e| e.spoken.starts_with("H3RENX "))
                .count(),
            1
        );
        h3_cleanup();
    }
}
