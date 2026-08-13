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
    /// (spoken trigger expanding to longer replacement text).
    /// Expansion entries are applied AFTER transcription and must
    /// NEVER enter the STT recognition prompt.
    #[serde(default = "default_entry_kind")]
    pub kind: String,
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
            let loaded = load_dictionary_internal().unwrap_or_default();
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
/// get fresh ids. Returns the merged list and the number actually added.
fn merge_dictionary_entries(
    existing: &[DictionaryEntry],
    incoming: Vec<DictionaryEntry>,
) -> (Vec<DictionaryEntry>, usize) {
    let mut entries = existing.to_vec();
    let mut added = 0;
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
        entries.push(entry);
        added += 1;
    }
    (entries, added)
}

// Tauri Commands

#[tauri::command]
pub fn get_dictionary() -> Result<Vec<DictionaryEntry>, String> {
    load_dictionary_internal().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_dictionary_entry(
    spoken: String,
    corrected: String,
    kind: Option<String>,
) -> Result<DictionaryEntry, String> {
    let (spoken, corrected) = normalize_entry_text(&spoken, &corrected)?;
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
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
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
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
    if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
        e.spoken = spoken;
        e.corrected = corrected;
        if let Some(kind) = kind {
            e.kind = kind;
        }
    }
    save_dictionary_internal(&entries).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn delete_dictionary_entry(id: String) -> Result<(), String> {
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    entries.retain(|e| e.id != id);
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
    }
}
