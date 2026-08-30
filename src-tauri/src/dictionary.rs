// Fluence Windows — Custom Dictionary module
// Stores spoken→corrected word/phrase pairs in a JSON file.
// Applied as post-processing after every transcription.

use anyhow::Result;
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

fn default_entry_kind() -> String {
    "correction".to_string()
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

fn dictionary_path() -> PathBuf {
    let mut path = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("dictionary.json");
    path
}

pub(crate) fn load_dictionary_internal() -> Result<Vec<DictionaryEntry>> {
    let path = dictionary_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path)?;
    let entries: Vec<DictionaryEntry> = serde_json::from_str(&data).unwrap_or_default();
    Ok(entries)
}

pub(crate) fn save_dictionary_internal(entries: &[DictionaryEntry]) -> Result<()> {
    let path = dictionary_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(entries)?;
    // Atomic write: tmp + sync_all + rename (prevents half-written file on power loss)
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &data)?;
    if let Ok(f) = fs::File::open(&tmp_path) {
        let _ = f.sync_all();
    }
    fs::rename(&tmp_path, &path)?;
    if let Ok(f) = fs::File::open(&path) {
        let _ = f.sync_all();
    }
    // Sync imports write through this function too. Drop compiled rules so
    // the next transcription uses the merged file immediately.
    invalidate_cache();
    Ok(())
}

fn cache_entries(entries: Vec<DictionaryEntry>) -> Vec<CachedEntry> {
    let mut cached = Vec::with_capacity(entries.len());
    for entry in entries {
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
/// get fresh ids and a creation timestamp. Returns the merged list and the
/// number actually added.
fn merge_dictionary_entries(
    existing: &[DictionaryEntry],
    incoming: Vec<DictionaryEntry>,
) -> (Vec<DictionaryEntry>, usize) {
    let mut entries = existing.to_vec();
    let mut added = 0;
    let now = chrono::Utc::now().timestamp_millis();
    for mut entry in incoming {
        entry.spoken = entry.spoken.trim().to_string();
        entry.corrected = entry.corrected.trim().to_string();
        if entry.spoken.is_empty() || entry.corrected.is_empty() {
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
    (entries, added)
}

/// Live entries only (not tombstoned) — the user-facing view (§30.2).
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

#[tauri::command]
pub fn add_dictionary_entry(
    spoken: String,
    corrected: String,
    kind: Option<String>,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
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
    // Monotonic per-account maxSeen (or global if no account yet) — prevents clock skew from making stale win
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
        kind: kind.unwrap_or_else(default_entry_kind),
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
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(entry)
}

#[tauri::command]
pub fn update_dictionary_entry(
    id: String,
    spoken: String,
    corrected: String,
    kind: Option<String>,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
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
    let mut found = false;
    for entry in all_entries.iter_mut() {
        if entry.id == id {
            if !crate::sync::metadata::belongs_to_account(
                entry.sync_account.as_deref(),
                active_account.as_deref(),
            ) {
                return Err("Dictionary entry belongs to another account".to_string());
            }
            if entry.deleted_at.is_some() {
                return Err("Cannot edit a deleted entry".to_string());
            }
            found = true;
            entry.spoken = spoken.clone();
            entry.corrected = corrected.clone();
            if let Some(k) = kind.clone() {
                entry.kind = k;
            }
            entry.updated_at = Some(now);
            entry.device_id = Some(device_id.clone());
            entry.dirty = true;
            // ever_pushed stays as is
            break;
        }
    }
    if !found {
        return Err("Dictionary entry not found".to_string());
    }
    // Update maxSeen persisted (use a dummy account hash for local clock)
    // We'll store maxSeen under a special key "__local__" or just update device's global
    // For now, we update per-account if we have account, else just don't persist maxSeen here (sync will handle)
    save_dictionary_internal(&all_entries).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn delete_dictionary_entry(
    id: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
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
    for entry in entries.iter_mut() {
        if entry.id == id {
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
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn import_dictionary(
    json_data: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<usize, String> {
    let _io = crate::sync::io_lock::io_lock_guard();
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
    let (merged, added) = merge_dictionary_entries(&existing, new_entries);
    save_dictionary_internal(&merged).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(added)
}

#[tauri::command]
pub fn export_dictionary() -> Result<String, String> {
    let active = crate::sync::metadata::current_account_hash();
    let entries: Vec<DictionaryEntry> = load_dictionary_internal()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|e| {
            crate::sync::metadata::belongs_to_account(
                e.sync_account.as_deref(),
                active.as_deref(),
            )
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
        let (merged, added) = merge_dictionary_entries(&existing, incoming);
        assert_eq!(added, 2);
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
}
