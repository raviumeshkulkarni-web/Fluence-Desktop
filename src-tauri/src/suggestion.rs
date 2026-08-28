// Fluence Windows — Suggestion Database
// Manages transcription correction suggestions.
// Suggestions are separate from the dictionary.
// The dictionary remains the only source of truth.
//
// Evidence → Hypothesis → Human Decision → Persistent Knowledge

use anyhow::Result;
use chrono::{DateTime, Utc};
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// Status of a suggestion in the review lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SuggestionStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "accepted")]
    Accepted,
    #[serde(rename = "dismissed")]
    Dismissed,
}

/// How the suggestion was discovered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SuggestionSource {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "manual")]
    Manual,
}

/// A single correction suggestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionEntry {
    pub id: String,
    /// What the STT heard (original text, not normalized)
    pub spoken: String,
    /// What it should be (final text, not normalized)
    pub corrected: String,
    /// How many times this correction pair was observed
    pub frequency: u32,
    /// ISO 8601 timestamp when first seen
    pub created_at: String,
    /// ISO 8601 timestamp when last seen
    pub last_seen: String,
    /// How the suggestion was discovered
    pub source: SuggestionSource,
    /// Current status in the review lifecycle
    pub status: SuggestionStatus,
}

/// The suggestion database with schema version for future migrations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionDatabase {
    /// Schema version for future migrations
    pub version: u32,
    /// All suggestions (pending, accepted, dismissed)
    pub suggestions: Vec<SuggestionEntry>,
}

impl Default for SuggestionDatabase {
    fn default() -> Self {
        Self {
            version: 1,
            suggestions: Vec::new(),
        }
    }
}

/// Mutex protects file operations (read-modify-write transactions).
static SUGGESTION_LOCK: Mutex<()> = Mutex::new(());

fn now_iso8601() -> String {
    Utc::now().to_rfc3339()
}

fn suggestions_path() -> PathBuf {
    let mut path = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("suggestions.json");
    path
}

/// Load the suggestion database from disk.
/// On corruption, renames to .corrupt.json and returns a fresh database.
fn load_from_disk() -> Result<SuggestionDatabase> {
    let path = suggestions_path();
    if !path.exists() {
        return Ok(SuggestionDatabase::default());
    }

    let data = fs::read_to_string(&path)?;

    match serde_json::from_str::<SuggestionDatabase>(&data) {
        Ok(db) => Ok(db),
        Err(e) => {
            // Rename corrupted file
            let corrupt_path = path.with_extension("json.corrupt.json");
            if let Err(rename_err) = fs::rename(&path, &corrupt_path) {
                log::error!("Failed to rename corrupt suggestions file: {}", rename_err);
            }

            log::warn!(
                "Corrupted suggestions file renamed to {:?}. Creating fresh database. Error: {}",
                corrupt_path,
                e
            );

            Ok(SuggestionDatabase::default())
        }
    }
}

/// Save the suggestion database to disk using atomic write.
/// Writes to .tmp file first, then renames to prevent corruption.
fn save_to_disk(database: &SuggestionDatabase) -> Result<()> {
    let path = suggestions_path();
    let tmp_path = path.with_extension("json.tmp");

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let data = serde_json::to_string_pretty(database)?;

    // Write to temp file
    fs::write(&tmp_path, &data)?;

    // Flush to disk
    fs::File::open(&tmp_path)?.sync_all()?;

    // Atomic rename
    fs::rename(&tmp_path, &path)?;

    Ok(())
}

/// A new suggestion should be skipped when the spoken→corrected pair is
/// already in the dictionary. The dictionary is the source of truth, so
/// accepted corrections must never be re-learned as pending suggestions.
fn should_skip_new_candidate(candidate_key: &str, dictionary_keys: &HashSet<String>) -> bool {
    dictionary_keys.contains(candidate_key)
}

/// Upsert suggestions from extracted candidates.
/// Repeated observations increment frequency and update last_seen.
/// Dismissed suggestions remain dismissed (user intent is respected).
/// Pairs already in the dictionary are skipped (never re-learned).
pub fn upsert_suggestions(candidates: Vec<crate::auto_learn::Candidate>) -> Result<(), String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let mut database = load_from_disk().map_err(|e| e.to_string())?;
    let now = now_iso8601();

    let dictionary_keys: HashSet<String> = crate::auto_learn::learner::get_current_dictionary()
        .into_iter()
        .collect();

    for candidate in candidates {
        let candidate_key =
            crate::dictionary::canonical_entry_key(&candidate.spoken, &candidate.corrected);

        // Find existing suggestion for this spoken→corrected pair,
        // using the canonical normalization path.
        let existing = database.suggestions.iter_mut().find(|s| {
            crate::dictionary::canonical_entry_key(&s.spoken, &s.corrected) == candidate_key
        });

        if let Some(suggestion) = existing {
            // Always update frequency and last_seen
            suggestion.frequency += 1;
            suggestion.last_seen = now.clone();

            // But do NOT reset Dismissed to Pending (respect user intent)
        } else if should_skip_new_candidate(&candidate_key, &dictionary_keys) {
            log::debug!(
                "Skipping new suggestion ({} chars → {} chars): already in dictionary",
                candidate.spoken.chars().count(),
                candidate.corrected.chars().count()
            );
        } else {
            // New suggestion
            database.suggestions.push(SuggestionEntry {
                id: uuid::Uuid::new_v4().to_string(),
                spoken: candidate.spoken,
                corrected: candidate.corrected,
                frequency: 1,
                created_at: now.clone(),
                last_seen: now.clone(),
                source: SuggestionSource::Auto,
                status: SuggestionStatus::Pending,
            });
        }
    }

    save_to_disk(&database).map_err(|e| e.to_string())?;
    Ok(())
}

/// Get all pending suggestions, ranked by frequency (desc) then recency (desc).
pub fn get_pending_suggestions() -> Result<Vec<SuggestionEntry>, String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let database = load_from_disk().map_err(|e| e.to_string())?;

    let mut pending: Vec<SuggestionEntry> = database
        .suggestions
        .into_iter()
        .filter(|s| s.status == SuggestionStatus::Pending)
        .collect();

    // Sort by frequency (desc), then last_seen (desc)
    pending.sort_by(|a, b| {
        b.frequency
            .cmp(&a.frequency)
            .then(b.last_seen.cmp(&a.last_seen))
    });

    Ok(pending)
}

/// Accept a suggestion: add to dictionary, mark as Accepted.
pub fn accept_suggestion(id: &str, scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>) -> Result<(), String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let mut database = load_from_disk().map_err(|e| e.to_string())?;

    // Find the suggestion
    let suggestion = database
        .suggestions
        .iter()
        .find(|s| s.id == id && s.status == SuggestionStatus::Pending)
        .ok_or_else(|| "Suggestion not found or not pending".to_string())?;

    let spoken = suggestion.spoken.clone();
    let corrected = suggestion.corrected.clone();

    // Add to dictionary (auto-learned suggestions are always corrections)
    crate::dictionary::add_dictionary_entry(spoken, corrected, None, scheduler)
        .map_err(|e| format!("Failed to add to dictionary: {}", e))?;

    // Mark as accepted (don't delete — keep for future analytics/undo)
    if let Some(suggestion) = database.suggestions.iter_mut().find(|s| s.id == id) {
        suggestion.status = SuggestionStatus::Accepted;
    }

    save_to_disk(&database).map_err(|e| e.to_string())?;
    Ok(())
}

/// Dismiss a suggestion.
pub fn dismiss_suggestion(id: &str) -> Result<(), String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let mut database = load_from_disk().map_err(|e| e.to_string())?;

    if let Some(suggestion) = database
        .suggestions
        .iter_mut()
        .find(|s| s.id == id && s.status == SuggestionStatus::Pending)
    {
        suggestion.status = SuggestionStatus::Dismissed;
    }

    save_to_disk(&database).map_err(|e| e.to_string())?;
    Ok(())
}

/// Clear all dismissed suggestions from the database.
pub fn clear_dismissed_suggestions() -> Result<(), String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let mut database = load_from_disk().map_err(|e| e.to_string())?;

    let before = database.suggestions.len();
    database
        .suggestions
        .retain(|s| s.status != SuggestionStatus::Dismissed);
    let after = database.suggestions.len();

    if before != after {
        save_to_disk(&database).map_err(|e| e.to_string())?;
        log::info!("Cleared {} dismissed suggestions", before - after);
    }

    Ok(())
}

/// Pure decision: is a suggestion with this `last_seen` timestamp stale
/// relative to the given cutoff? Unparseable timestamps are treated as
/// not expired (safe default).
fn is_expired(last_seen: &str, cutoff: &DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(last_seen)
        .map(|t| t < *cutoff)
        .unwrap_or(false)
}

/// Expire stale suggestions that haven't been seen in 30 days.
/// Run at application startup and when suggestions page is opened.
pub fn expire_stale_suggestions() -> Result<u32, String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let mut database = load_from_disk().map_err(|e| e.to_string())?;

    let cutoff = Utc::now() - chrono::Duration::days(30);
    let mut expired_count = 0;

    for suggestion in &mut database.suggestions {
        if suggestion.status != SuggestionStatus::Pending {
            continue;
        }

        if is_expired(&suggestion.last_seen, &cutoff) {
            suggestion.status = SuggestionStatus::Dismissed;
            expired_count += 1;
        }
    }

    if expired_count > 0 {
        save_to_disk(&database).map_err(|e| e.to_string())?;
        log::info!("Expired {} stale suggestions", expired_count);
    }

    Ok(expired_count)
}

// ── Tauri Commands ───────────────────────────────────────────────

#[tauri::command]
pub fn get_suggestions() -> Result<Vec<SuggestionEntry>, String> {
    get_pending_suggestions()
}

#[tauri::command]
pub fn accept_suggestion_command(id: String, scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>) -> Result<(), String> {
    accept_suggestion(&id, scheduler)
}

#[tauri::command]
pub fn dismiss_suggestion_command(id: String) -> Result<(), String> {
    dismiss_suggestion(&id)
}

#[tauri::command]
pub fn clear_dismissed_suggestions_command() -> Result<(), String> {
    clear_dismissed_suggestions()
}

#[tauri::command]
pub fn expire_stale_suggestions_command() -> Result<u32, String> {
    expire_stale_suggestions()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suggestion_database_default() {
        let db = SuggestionDatabase::default();
        assert_eq!(db.version, 1);
        assert!(db.suggestions.is_empty());
    }

    #[test]
    fn test_suggestion_status_serialization() {
        let pending = SuggestionStatus::Pending;
        let json = serde_json::to_string(&pending).unwrap();
        assert_eq!(json, "\"pending\"");

        let accepted = SuggestionStatus::Accepted;
        let json = serde_json::to_string(&accepted).unwrap();
        assert_eq!(json, "\"accepted\"");

        let dismissed = SuggestionStatus::Dismissed;
        let json = serde_json::to_string(&dismissed).unwrap();
        assert_eq!(json, "\"dismissed\"");
    }

    #[test]
    fn test_suggestion_source_serialization() {
        let auto = SuggestionSource::Auto;
        let json = serde_json::to_string(&auto).unwrap();
        assert_eq!(json, "\"auto\"");
    }

    #[test]
    fn test_should_skip_new_candidate_skips_dictionary_pairs() {
        let mut dictionary_keys = HashSet::new();
        dictionary_keys.insert(crate::dictionary::canonical_entry_key("shunade", "Sinead"));

        // Exact pair already in dictionary → skip
        assert!(should_skip_new_candidate(
            &crate::dictionary::canonical_entry_key("shunade", "Sinead"),
            &dictionary_keys
        ));
        // Case difference still matches the canonical key → skip
        assert!(should_skip_new_candidate(
            &crate::dictionary::canonical_entry_key("SHUNADE", "sinead"),
            &dictionary_keys
        ));
        // Different corrected word → not in dictionary → do not skip
        assert!(!should_skip_new_candidate(
            &crate::dictionary::canonical_entry_key("shunade", "Shanade"),
            &dictionary_keys
        ));
        // Different spoken word → not in dictionary → do not skip
        assert!(!should_skip_new_candidate(
            &crate::dictionary::canonical_entry_key("shanade", "Sinead"),
            &dictionary_keys
        ));
    }

    #[test]
    fn test_is_expired_before_cutoff() {
        let cutoff: DateTime<Utc> = DateTime::parse_from_rfc3339("2024-01-15T00:00:00Z")
            .unwrap()
            .into();
        assert!(is_expired("2024-01-14T00:00:00Z", &cutoff));
    }

    #[test]
    fn test_is_expired_after_cutoff() {
        let cutoff: DateTime<Utc> = DateTime::parse_from_rfc3339("2024-01-15T00:00:00Z")
            .unwrap()
            .into();
        assert!(!is_expired("2024-01-16T00:00:00Z", &cutoff));
    }

    #[test]
    fn test_is_expired_unparseable_treated_as_fresh() {
        assert!(!is_expired("not-a-timestamp", &Utc::now()));
    }

    #[test]
    fn test_is_expired_at_cutoff_boundary_not_strictly_older() {
        // Exactly at the cutoff is not strictly older → not expired.
        let cutoff: DateTime<Utc> = DateTime::parse_from_rfc3339("2024-01-15T00:00:00Z")
            .unwrap()
            .into();
        assert!(!is_expired("2024-01-15T00:00:00Z", &cutoff));
        assert!(is_expired("2024-01-14T23:59:59Z", &cutoff));
    }
}
