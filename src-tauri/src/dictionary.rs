// Fluence Windows — Custom Dictionary module
// Stores spoken→corrected word/phrase pairs in a JSON file.
// Applied as post-processing after every transcription.

use anyhow::Result;
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub id: String,
    pub spoken: String,
    pub corrected: String,
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

// Tauri Commands

#[tauri::command]
pub fn get_dictionary() -> Result<Vec<DictionaryEntry>, String> {
    load_dictionary_internal().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_dictionary_entry(spoken: String, corrected: String) -> Result<DictionaryEntry, String> {
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    let entry = DictionaryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        spoken,
        corrected,
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
) -> Result<(), String> {
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
        e.spoken = spoken;
        e.corrected = corrected;
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
    let mut entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    let count = new_entries.len();
    for mut entry in new_entries {
        entry.id = uuid::Uuid::new_v4().to_string();
        entries.push(entry);
    }
    save_dictionary_internal(&entries).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(count)
}

#[tauri::command]
pub fn export_dictionary() -> Result<String, String> {
    let entries = load_dictionary_internal().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
}
