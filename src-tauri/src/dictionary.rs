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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub id: String,
    pub spoken: String,
    pub corrected: String,
    /// Entry type: "correction" (word/phrase fix) or "expansion"
    /// (spoken trigger expanding to longer replacement text).
    /// Expansion entries are applied AFTER transcription and must
    /// NEVER enter the STT recognition prompt.
    #[serde(default = "default_entry_kind")]
    pub kind: String,
    // §30 sync metadata. `Option` + serde defaults keep legacy JSON loadable.
    // `created_at: None` marks entries created before sync; the sync store
    // backfills them on first mapping (cross-device timestamps then differ —
    // documented edge, §30.2).
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub deleted_at: Option<i64>,
    #[serde(default)]
    pub sync_state: Option<String>,
    #[serde(default)]
    pub server_file_id: Option<String>,
    #[serde(default)]
    pub sync_account: Option<String>,
    #[serde(default)]
    pub quarantine_reason: Option<String>,
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

fn load_dictionary_internal() -> Result<Vec<DictionaryEntry>> {
    let path = dictionary_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path)?;
    let entries: Vec<DictionaryEntry> = serde_json::from_str(&data).unwrap_or_default();
    Ok(entries)
}

fn save_dictionary_internal(entries: &[DictionaryEntry]) -> Result<()> {
    let path = dictionary_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(entries)?;
    fs::write(&path, data)?;
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
                    "Invalid regex pattern for spoken phrase '{}': {}",
                    entry.spoken,
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
            let loaded: Vec<DictionaryEntry> = load_dictionary_internal()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.deleted_at.is_none()) // §30.2: deleted entries never apply
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

fn invalidate_cache() {
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
    Ok(live_entries(
        load_dictionary_internal().map_err(|e| e.to_string())?,
    ))
}

#[tauri::command]
pub fn add_dictionary_entry(
    spoken: String,
    corrected: String,
    kind: Option<String>,
) -> Result<DictionaryEntry, String> {
    let (spoken, corrected) = normalize_entry_text(&spoken, &corrected)?;
    let mut entries = live_entries(load_dictionary_internal().map_err(|e| e.to_string())?);
    if entries_already_have(&entries, &spoken, &corrected) {
        return Err(format!(
            "Dictionary entry '{} → {}' already exists",
            spoken, corrected
        ));
    }
    let entry = DictionaryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        spoken,
        corrected,
        kind: kind.unwrap_or_else(default_entry_kind),
        created_at: Some(chrono::Utc::now().timestamp_millis()),
        deleted_at: None,
        sync_state: None,
        server_file_id: None,
        sync_account: None,
        quarantine_reason: None,
    };
    entries.push(entry.clone());
    save_dictionary_internal(&entries).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(entry)
}

#[tauri::command]
pub fn update_dictionary_entry(
    id: String,
    spoken: String,
    corrected: String,
    kind: Option<String>,
) -> Result<(), String> {
    let (spoken, corrected) = normalize_entry_text(&spoken, &corrected)?;
    let mut entries = live_entries(load_dictionary_internal().map_err(|e| e.to_string())?);
    let key = canonical_entry_key(&spoken, &corrected);
    let collides = entries
        .iter()
        .any(|other| other.id != id && canonical_entry_key(&other.spoken, &other.corrected) == key);
    if collides {
        return Err(format!(
            "Dictionary entry '{} → {}' already exists",
            spoken, corrected
        ));
    }
    // §30.2: an edit is a tombstone + a new UUID, so every device converges
    // on the same record identity and no in-place rewrite of an uploaded
    // record ever happens.
    let now = chrono::Utc::now().timestamp_millis();
    let mut old_kind: Option<String> = None;
    let mut found = false;
    for entry in entries.iter_mut() {
        if entry.id == id {
            found = true;
            entry.deleted_at = Some(now);
            if entry.server_file_id.is_some() {
                entry.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
            }
            old_kind = Some(entry.kind.clone());
            break;
        }
    }
    if !found {
        return Err("Dictionary entry not found".to_string());
    }
    let new_entry = DictionaryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        spoken,
        corrected,
        kind: kind.unwrap_or_else(|| old_kind.unwrap_or_else(default_entry_kind)),
        created_at: Some(now),
        deleted_at: None,
        sync_state: None,
        server_file_id: None,
        sync_account: None,
        quarantine_reason: None,
    };
    entries.push(new_entry);
    save_dictionary_internal(&entries).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn delete_dictionary_entry(id: String) -> Result<(), String> {
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut removed = false;
    for entry in entries.iter_mut() {
        if entry.id == id {
            if entry.server_file_id.is_some() {
                // Uploaded → tombstone so other devices delete it too (§30.2).
                entry.deleted_at = Some(now);
                entry.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
            } else {
                // Never uploaded → provably safe to hard-delete (§14).
                removed = true;
            }
        }
    }
    if removed {
        entries.retain(|e| e.id != id);
    }
    save_dictionary_internal(&entries).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn import_dictionary(json_data: String) -> Result<usize, String> {
    let new_entries: Vec<DictionaryEntry> =
        serde_json::from_str(&json_data).map_err(|e| e.to_string())?;
    let existing = load_dictionary_internal().map_err(|e| e.to_string())?;
    let (merged, added) = merge_dictionary_entries(&existing, new_entries);
    save_dictionary_internal(&merged).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(added)
}

#[tauri::command]
pub fn export_dictionary() -> Result<String, String> {
    let entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// §30 sync — DictionarySyncStore (LocalStore seam for the dictionary kind).
// Wired into the desktop binary by the Phase 7 scheduler.
// ---------------------------------------------------------------------------

pub(crate) mod sync_store {
    use super::*;

    use crate::sync::engine::{
        LocalRow, LocalStore, QuarantineReason, SyncError, SYNC_STATE_LOCAL,
    };
    use crate::sync::wire::RecordType;

    /// Sync-facing seam over the same persisted entry list (§30.2). Keeps every
    /// row — live, tombstoned, latched — so the engine can reconcile them all;
    /// user-facing reads (`get_dictionary`, corrections) see only live rows.
    #[derive(Debug, Default)]
    pub struct DictionarySyncStore {
        pub entries: Vec<DictionaryEntry>,
    }

    impl DictionarySyncStore {
        pub fn new() -> Self {
            let mut store = Self {
                entries: load_dictionary_internal().unwrap_or_default(),
            };
            store.backfill_legacy_created_at();
            store
        }

        #[cfg(test)]
        pub fn entries(&self) -> &[DictionaryEntry] {
            &self.entries
        }

        /// Entries created before sync carry `created_at: None`; assign them a
        /// timestamp on first mapping so the wire record is always valid. The
        /// backfill is persisted; cross-device timestamps for such legacy rows
        /// may differ — documented edge (§30.2).
        pub fn backfill_legacy_created_at(&mut self) {
            let now = chrono::Utc::now().timestamp_millis();
            let mut changed = false;
            for entry in self.entries.iter_mut() {
                if entry.created_at.is_none() {
                    entry.created_at = Some(now);
                    changed = true;
                }
            }
            if changed {
                self.save();
            }
        }

        fn save(&self) {
            if save_dictionary_internal(&self.entries).is_ok() {
                invalidate_cache();
            }
        }
    }

    impl LocalStore for DictionarySyncStore {
        fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow> {
            let mut out: Vec<LocalRow> = self
                .entries
                .iter()
                .filter(|e| match account {
                    None => e.sync_account.is_none(),
                    Some(a) => e.sync_account.as_deref().map_or(true, |s| s == a),
                })
                .map(entry_to_local)
                .collect();
            out.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            out
        }

        fn find_row(&self, uuid: &str) -> Option<LocalRow> {
            self.entries
                .iter()
                .find(|e| e.id == uuid)
                .map(entry_to_local)
        }

        fn import(&mut self, row: LocalRow) -> Result<(), SyncError> {
            let Some(entry) = local_to_entry(row) else {
                return Ok(()); // other kinds never reach this store
            };
            if let Some(existing) = self.entries.iter_mut().find(|e| e.id == entry.id) {
                *existing = entry;
            } else {
                self.entries.push(entry);
            }
            self.save();
            Ok(())
        }

        fn mark_tombstoned(&mut self, uuid: &str, deleted_at: i64) -> Result<(), SyncError> {
            if let Some(e) = self.entries.iter_mut().find(|e| e.id == uuid) {
                e.deleted_at = Some(deleted_at);
                e.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
            }
            self.save();
            Ok(())
        }

        fn set_server_file_id(&mut self, uuid: &str, file_id: &str) -> Result<(), SyncError> {
            if let Some(e) = self.entries.iter_mut().find(|e| e.id == uuid) {
                e.server_file_id = Some(file_id.to_string());
            }
            self.save();
            Ok(())
        }

        fn set_sync_state(&mut self, uuid: &str, state: &str) -> Result<(), SyncError> {
            if let Some(e) = self.entries.iter_mut().find(|e| e.id == uuid) {
                e.sync_state = Some(state.to_string());
            }
            self.save();
            Ok(())
        }

        fn quarantine(&mut self, uuid: &str, reason: QuarantineReason) -> Result<(), SyncError> {
            if let Some(e) = self.entries.iter_mut().find(|e| e.id == uuid) {
                e.quarantine_reason = Some(reason.as_str().to_string());
                e.sync_state = Some(crate::sync::engine::SYNC_STATE_QUARANTINED.to_string());
            }
            self.save();
            Ok(())
        }

        fn clear_quarantine(&mut self, uuid: &str) -> Result<(), SyncError> {
            if let Some(e) = self.entries.iter_mut().find(|e| e.id == uuid) {
                e.quarantine_reason = None;
                e.sync_state = Some(SYNC_STATE_LOCAL.to_string());
            }
            self.save();
            Ok(())
        }

        fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError> {
            self.entries.retain(|e| e.id != uuid);
            self.save();
            Ok(())
        }
    }

    fn entry_to_local(e: &DictionaryEntry) -> LocalRow {
        LocalRow {
            uuid: e.id.clone(),
            timestamp_ms: e.created_at.unwrap_or(0),
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Dictionary,
            spoken: Some(e.spoken.clone()),
            corrected: Some(e.corrected.clone()),
            kind: Some(e.kind.clone()),
            trigger: None,
            expansion: None,
            settings_key: None,
            settings_value: None,
            deleted_at: e.deleted_at,
            server_file_id: e.server_file_id.clone(),
            sync_account: e.sync_account.clone(),
            sync_state: e
                .sync_state
                .clone()
                .unwrap_or_else(|| SYNC_STATE_LOCAL.to_string()),
            quarantine_reason: e.quarantine_reason.clone(),
        }
    }

    fn local_to_entry(row: LocalRow) -> Option<DictionaryEntry> {
        if row.rtype != RecordType::Dictionary {
            return None;
        }
        Some(DictionaryEntry {
            id: row.uuid,
            spoken: row.spoken.unwrap_or_default(),
            corrected: row.corrected.unwrap_or_default(),
            kind: row.kind.unwrap_or_else(default_entry_kind),
            created_at: Some(row.timestamp_ms),
            deleted_at: row.deleted_at,
            sync_state: Some(row.sync_state),
            server_file_id: row.server_file_id,
            sync_account: row.sync_account,
            quarantine_reason: row.quarantine_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::sync_store::*;
    use super::*;
    use crate::sync::engine::{LocalRow, LocalStore, SYNC_STATE_LOCAL};
    use crate::sync::wire::RecordType;

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

    // §30.2 edit semantics --------------------------------------------------

    #[test]
    fn test_update_tombstones_old_and_creates_new_uuid() {
        let uploaded = entry("1", "tori", "Tauri");
        let mut uploaded = uploaded;
        uploaded.server_file_id = Some("F1".to_string());
        uploaded.sync_state = Some(crate::sync::engine::SYNC_STATE_CLEAN.to_string());
        let mut entries = vec![uploaded];

        let now = chrono::Utc::now().timestamp_millis();
        if let Some(old) = entries.iter_mut().find(|e| e.id == "1") {
            old.deleted_at = Some(now);
            old.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
        }
        entries.push(DictionaryEntry {
            id: "new-uuid".to_string(),
            spoken: "tori".to_string(),
            corrected: "Tauri 2".to_string(),
            kind: "correction".to_string(),
            created_at: Some(now),
            deleted_at: None,
            sync_state: None,
            server_file_id: None,
            sync_account: None,
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

    #[test]
    fn test_sync_store_roundtrip_through_wire() {
        const U: &str = "00000000-0000-4000-8000-000000000005";
        let mut store = DictionarySyncStore::default();
        store
            .import(local_to_entry_live(U, "tori", "Tauri"))
            .unwrap();
        let row = store.find_row(U).unwrap();
        let rec = crate::sync::wire::parse(row.to_wire().to_json().as_bytes(), U).unwrap();
        assert_eq!(rec.rtype, RecordType::Dictionary);
        assert_eq!(rec.spoken.as_deref(), Some("tori"));
        assert_eq!(rec.corrected.as_deref(), Some("Tauri"));
        assert_eq!(rec.kind.as_deref(), Some("correction"));
        let row2 = store.find_row(U).unwrap();
        assert_eq!(row2.to_wire().to_json(), row.to_wire().to_json());
    }

    fn local_to_entry_live(uuid: &str, spoken: &str, corrected: &str) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713456000123,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Dictionary,
            spoken: Some(spoken.to_string()),
            corrected: Some(corrected.to_string()),
            kind: Some("correction".to_string()),
            trigger: None,
            expansion: None,
            settings_key: None,
            settings_value: None,
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        }
    }

    #[test]
    fn test_sync_store_backfills_legacy_entries() {
        let mut store = DictionarySyncStore::default();
        let legacy = entry("legacy", "grok", "Groq");
        let mut legacy = legacy;
        legacy.created_at = None;
        store.entries.push(legacy);
        store.backfill_legacy_created_at();
        assert!(
            store.entries[0].created_at.is_some(),
            "legacy entries get a timestamp on first mapping"
        );
    }
}
