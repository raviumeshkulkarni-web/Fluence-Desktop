// Fluence Windows - Suggestion Database
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
    /// RFC3339 timestamp of automatic acceptance, if auto-accepted.
    /// Set ONLY by the auto-accept path (manual accepts leave None) and
    /// used solely for the daily auto-accept rate cap. Local-only file;
    /// never synced. Absent on legacy rows (treated as outside any window).
    #[serde(default)]
    pub accepted_at: Option<String>,
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

    // Flush to disk (best-effort: Windows denies sync on read-only handles,
    // so a sync failure must never fail the save - matches dictionary.rs,
    // settings.rs, snippets.rs and audit_evidence.rs).
    if let Ok(f) = fs::File::open(&tmp_path) {
        let _ = f.sync_all();
    }

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
                accepted_at: None,
            });
        }
    }

    save_to_disk(&database).map_err(|e| e.to_string())?;
    // Auto-accept evaluation: every learning path converges here. The lock
    // is released first - evaluation re-locks in suggestions → dictionary
    // order, matching manual accept, so no lock inversion. Evaluation can
    // never fail the upsert: learning must survive auto-accept bugs.
    drop(_guard);
    if let Err(e) = evaluate_auto_accept() {
        log::warn!("[AutoLearn] Auto-accept evaluation failed: {}", e);
    }
    Ok(())
}

/// Minimum observations before a suggestion surfaces to the user.
/// Recording starts at frequency 1, but surfacing waits for a repeat:
/// systematic STT mishearings recur while one-off typos and mid-typing
/// states do not. Below-threshold rows keep counting invisibly.
pub const MIN_OBSERVATIONS_TO_SURFACE: u32 = 2;

/// True when a suggestion has been observed often enough to surface.
/// Dismissed and accepted rows never resurface regardless of frequency.
fn is_ready_to_surface(entry: &SuggestionEntry) -> bool {
    entry.status == SuggestionStatus::Pending && entry.frequency >= MIN_OBSERVATIONS_TO_SURFACE
}

/// Get all pending suggestions, ranked by frequency (desc) then recency (desc).
/// Only rows observed at least MIN_OBSERVATIONS_TO_SURFACE times are
/// returned; first sightings stay in the database and keep counting.
pub fn get_pending_suggestions() -> Result<Vec<SuggestionEntry>, String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;

    let database = load_from_disk().map_err(|e| e.to_string())?;

    let mut pending: Vec<SuggestionEntry> = database
        .suggestions
        .into_iter()
        .filter(is_ready_to_surface)
        .collect();

    // Sort by frequency (desc), then last_seen (desc)
    pending.sort_by(|a, b| {
        b.frequency
            .cmp(&a.frequency)
            .then(b.last_seen.cmp(&a.last_seen))
    });

    Ok(pending)
}

/// Observations required before a suggestion may auto-accept.
pub const AUTO_ACCEPT_MIN_OBSERVATIONS: u32 = 3;

/// Spoken-key occupancy in the dictionary for the auto-accept decision.
#[derive(Debug, PartialEq, Eq)]
enum SpokenKeyState {
    /// No live or deleted row carries this spoken key.
    Free,
    /// A live row carries it - accepting would collide.
    LiveTaken,
    /// Only deleted (tombstoned) rows carry it - accepting would resurrect
    /// a user-deleted entry.
    Tombstoned,
}

/// Classify a spoken key against live and tombstoned dictionary rows.
/// Normalization and account scoping mirror `add_dictionary_entry` exactly.
///
/// Note the documented residual on `dismiss_matching_suggestions`: a
/// hard-deleted never-pushed key with no suggestion row reads as Free
/// here. Only live rows and tombstones are visible to this check.
fn spoken_key_state(
    entries: &[crate::dictionary::DictionaryEntry],
    spoken: &str,
    active_account: Option<&str>,
) -> SpokenKeyState {
    let bk = spoken.trim().to_lowercase();
    let mut tombstoned = false;
    for e in entries {
        if !crate::sync::metadata::belongs_to_account(e.sync_account.as_deref(), active_account) {
            continue;
        }
        if e.spoken.trim().to_lowercase() != bk {
            continue;
        }
        if e.deleted_at.is_none() {
            return SpokenKeyState::LiveTaken;
        }
        tombstoned = true;
    }
    if tombstoned {
        SpokenKeyState::Tombstoned
    } else {
        SpokenKeyState::Free
    }
}

/// True when first and last observation fall on different calendar days.
/// Stamps are UTC RFC3339; comparison is by calendar date, not a hard 24h
/// window, so an evening observation plus a morning one qualifies.
/// Unparseable stamps fail closed. See task notes for the rationale:
/// systematic mishearings recur across days, bursts do not.
fn observed_on_different_days(created_at: &str, last_seen: &str) -> bool {
    let created = DateTime::parse_from_rfc3339(created_at).map(|t| t.date_naive());
    let seen = DateTime::parse_from_rfc3339(last_seen).map(|t| t.date_naive());
    match (created, seen) {
        (Ok(c), Ok(s)) => c != s,
        _ => false,
    }
}

/// Evaluate pending suggestions for automatic acceptance.
/// Runs at the end of every upsert (all learning paths converge there) but
/// is a no-op unless the local-only `auto_accept_enabled` setting is ON
/// (default OFF). Decision order per candidate: Pending-only →
/// spoken-key free (live and tombstoned) → frequency + calendar-day span.
/// The tempo gate (repeat observations across days) is the anti-burst
/// control; there is deliberately no per-day cap. Every accept and every
/// skip is logged (word lengths only, never content - see the
/// no-user-content logging policy). Never fails the upsert: the caller
/// logs and swallows errors.
pub(crate) fn evaluate_auto_accept() -> Result<usize, String> {
    // Backend guarantee: inert unless BOTH learns are on. Auto-accept
    // without auto-learn is meaningless (nothing would ever be observed),
    // and the UI disables the toggle in that state; this check makes the
    // invariant hold regardless of which path invokes evaluation.
    let enabled = crate::settings::load_settings()
        .map(|s| s.auto_learn_enabled && s.auto_accept_enabled)
        .unwrap_or(false);
    if !enabled {
        return Ok(0);
    }
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
    let mut database = load_from_disk().map_err(|e| e.to_string())?;
    let active_account = crate::sync::metadata::current_account_hash();
    let mut dict_entries =
        crate::dictionary::load_dictionary_internal().map_err(|e| e.to_string())?;
    // Dismissed rows meeting the bar are logged (deny-remember working)
    // but never touched: dismissal is a permanent human veto.
    for s in database.suggestions.iter().filter(|s| {
        s.status == SuggestionStatus::Dismissed
            && s.frequency >= AUTO_ACCEPT_MIN_OBSERVATIONS
            && observed_on_different_days(&s.created_at, &s.last_seen)
    }) {
        log::info!(
            "[AutoLearn] Auto-accept suppressed (dismissed): {} chars -> {} chars ({}x)",
            s.spoken.chars().count(),
            s.corrected.chars().count(),
            s.frequency
        );
    }
    // Snapshot qualifying ids first: accepting mutates the database.
    let candidates: Vec<String> = database
        .suggestions
        .iter()
        .filter(|s| s.status == SuggestionStatus::Pending)
        .filter(|s| {
            if s.frequency < AUTO_ACCEPT_MIN_OBSERVATIONS {
                log::info!(
                    "[AutoLearn] Auto-accept skipped (below threshold {}/{}): {} chars -> {} chars",
                    s.frequency,
                    AUTO_ACCEPT_MIN_OBSERVATIONS,
                    s.spoken.chars().count(),
                    s.corrected.chars().count()
                );
                false
            } else {
                true
            }
        })
        .filter(|s| {
            if !observed_on_different_days(&s.created_at, &s.last_seen) {
                log::info!(
                    "[AutoLearn] Auto-accept skipped (single-day span): {} chars -> {} chars ({}x)",
                    s.spoken.chars().count(),
                    s.corrected.chars().count(),
                    s.frequency
                );
                false
            } else {
                true
            }
        })
        .map(|s| s.id.clone())
        .collect();
    let mut accepted = 0usize;
    for id in candidates {
        let (spoken, corrected) = match database.suggestions.iter().find(|s| s.id == id) {
            Some(s) => {
                match spoken_key_state(&dict_entries, &s.spoken, active_account.as_deref()) {
                    SpokenKeyState::Free => (s.spoken.clone(), s.corrected.clone()),
                    SpokenKeyState::LiveTaken => {
                        log::info!(
                            "[AutoLearn] Auto-accept skipped (spoken key taken): {} chars",
                            s.spoken.chars().count()
                        );
                        continue;
                    }
                    SpokenKeyState::Tombstoned => {
                        log::info!(
                            "[AutoLearn] Auto-accept skipped (tombstoned key): {} chars",
                            s.spoken.chars().count()
                        );
                        continue;
                    }
                }
            }
            None => continue,
        };
        match crate::dictionary::add_dictionary_entry_internal(
            spoken.clone(),
            corrected.clone(),
            None,
        ) {
            Ok(_) => {
                let stamp = Utc::now().to_rfc3339();
                if let Some(s) = database.suggestions.iter_mut().find(|s| s.id == id) {
                    s.status = SuggestionStatus::Accepted;
                    s.accepted_at = Some(stamp);
                }
                // Refresh the snapshot so a second suggestion sharing this
                // spoken key (different correction) sees the taken key.
                if let Ok(reloaded) = crate::dictionary::load_dictionary_internal() {
                    dict_entries = reloaded;
                }
                accepted += 1;
                log::info!(
                    "[AutoLearn] Auto-accepted suggestion: {} chars -> {} chars",
                    spoken.chars().count(),
                    corrected.chars().count()
                );
            }
            Err(e) => {
                // Typically a raced duplicate spoken key: skip, converge next round.
                log::info!(
                    "[AutoLearn] Auto-accept skipped (add failed): {} chars -> {} chars: {}",
                    spoken.chars().count(),
                    corrected.chars().count(),
                    e
                );
            }
        }
    }
    if accepted > 0 {
        save_to_disk(&database).map_err(|e| e.to_string())?;
    }
    Ok(accepted)
}

/// Mark suggestion rows matching a spoken→corrected pair as Dismissed.
/// Called when the corresponding dictionary entry is deleted, so the pair
/// can never auto-resurface: re-observation bumps frequency on a Dismissed
/// row (never reset to Pending), and the auto path only considers Pending.
///
/// DOCUMENTED RESIDUAL (key-scoped deny gap): this linkage is pair-scoped
/// and only fires when a suggestion row exists for the pair. Deleting a
/// never-pushed entry that was added manually (or otherwise has no
/// suggestion row) leaves zero trace - no tombstone (hard delete), no
/// deny record - so a later ≥3x cross-day observation reads the spoken key
/// as Free and may auto-accept it. This matches existing manual semantics
/// (a hard-deleted key can always return via a later add); auto-accept just
/// makes the return automatic. A true key-scoped deny needs new state (a
/// key-level tombstone table or key-only Dismissed slot) - schema change,
/// deliberately out of scope.
///
/// Accepted rows are also revoked to Dismissed - the user just revoked the
/// accept. Never fails the dictionary delete: the caller logs and continues.
pub(crate) fn dismiss_matching_suggestions(spoken: &str, corrected: &str) -> Result<usize, String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
    let mut database = load_from_disk().map_err(|e| e.to_string())?;
    let key = crate::dictionary::canonical_entry_key(spoken, corrected);
    let mut dismissed = 0;
    for s in database.suggestions.iter_mut() {
        if s.status != SuggestionStatus::Dismissed
            && crate::dictionary::canonical_entry_key(&s.spoken, &s.corrected) == key
        {
            s.status = SuggestionStatus::Dismissed;
            dismissed += 1;
        }
    }
    if dismissed > 0 {
        save_to_disk(&database).map_err(|e| e.to_string())?;
        log::info!(
            "[AutoLearn] Dismissed {} suggestion(s) matching deleted dictionary entry",
            dismissed
        );
    }
    Ok(dismissed)
}

/// Accept a suggestion: add to dictionary, mark as Accepted.
pub fn accept_suggestion(
    id: &str,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
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

    // Mark as accepted (don't delete - keep for future analytics/undo)
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

/// Count dismissed rows (deny memory) in the database.
fn count_dismissed(database: &SuggestionDatabase) -> usize {
    database
        .suggestions
        .iter()
        .filter(|s| s.status == SuggestionStatus::Dismissed)
        .count()
}

/// Clear all dismissed suggestions from the database.
/// Returns how many rows were actually removed (0 = nothing to clear).
pub fn clear_dismissed_suggestions() -> Result<usize, String> {
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

    Ok(before - after)
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
pub fn accept_suggestion_command(
    id: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    accept_suggestion(&id, scheduler)
}

#[tauri::command]
pub fn dismiss_suggestion_command(id: String) -> Result<(), String> {
    dismiss_suggestion(&id)
}

#[tauri::command]
pub fn clear_dismissed_suggestions_command() -> Result<usize, String> {
    clear_dismissed_suggestions()
}

/// Number of dismissed rows, so the UI can disable "Clear Dismissed" when
/// there is nothing to clear instead of toasting success for a no-op.
#[tauri::command]
pub fn get_dismissed_count() -> Result<usize, String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
    let database = load_from_disk().map_err(|e| e.to_string())?;
    Ok(count_dismissed(&database))
}

#[tauri::command]
pub fn expire_stale_suggestions_command() -> Result<u32, String> {
    expire_stale_suggestions()
}

/// Sub-threshold pending rows for the auto-mode "learning…" state:
/// observed but not yet actionable (below the surface gate). Dismissible
/// as a permanent per-pair veto before anything can promote.
#[tauri::command]
pub fn get_learning_suggestions() -> Result<Vec<SuggestionEntry>, String> {
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
    let database = load_from_disk().map_err(|e| e.to_string())?;
    let mut learning: Vec<SuggestionEntry> = database
        .suggestions
        .into_iter()
        .filter(|s| s.status == SuggestionStatus::Pending && !is_ready_to_surface(s))
        .collect();
    learning.sort_by(|a, b| {
        b.frequency
            .cmp(&a.frequency)
            .then(b.last_seen.cmp(&a.last_seen))
    });
    Ok(learning)
}

/// True when a row belongs in the "auto-added" provenance set: accepted by
/// the auto path (stamped), not merely manually accepted. Used for the
/// dictionary provenance badge; the UI no longer renders a separate
/// auto-added group.
fn is_auto_added(row: &SuggestionEntry) -> bool {
    row.status == SuggestionStatus::Accepted
        && row.source == SuggestionSource::Auto
        && row.accepted_at.is_some()
}

/// Recently auto-accepted rows (provenance source for the dictionary
/// "Added" badge). Most recent first, capped - the full history stays in
/// the file. No UI group renders this; it exists so the dictionary page
/// can badge auto-added rows without re-deriving trigger state.
#[tauri::command]
pub fn get_auto_accepted_suggestions() -> Result<Vec<SuggestionEntry>, String> {
    const AUTO_ADDED_FEED_CAP: usize = 50;
    let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
    let database = load_from_disk().map_err(|e| e.to_string())?;
    let mut accepted: Vec<SuggestionEntry> = database
        .suggestions
        .into_iter()
        .filter(is_auto_added)
        .collect();
    accepted.sort_by(|a, b| {
        b.accepted_at
            .cmp(&a.accepted_at)
            .then(b.last_seen.cmp(&a.last_seen))
    });
    accepted.truncate(AUTO_ADDED_FEED_CAP);
    Ok(accepted)
}

/// Revert one auto-added pair: remove its dictionary row (which fires the
/// delete→dismiss linkage, recording a permanent veto) and report.
/// Restricted to Auto+Accepted rows - manual accepts revert via the normal
/// dictionary Delete path, unchanged.
#[tauri::command]
pub fn revert_auto_accepted_suggestion(id: String) -> Result<(), String> {
    let (spoken, dict_id) = {
        let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
        let database = load_from_disk().map_err(|e| e.to_string())?;
        let row = database
            .suggestions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| "Suggestion not found".to_string())?;
        if row.source != SuggestionSource::Auto || row.status != SuggestionStatus::Accepted {
            return Err("Only auto-added suggestions can be reverted here".to_string());
        }
        let active = crate::sync::metadata::current_account_hash();
        let bk = row.spoken.trim().to_lowercase();
        let dict_id = crate::dictionary::load_dictionary_internal()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|e| e.deleted_at.is_none())
            .filter(|e| {
                crate::sync::metadata::belongs_to_account(
                    e.sync_account.as_deref(),
                    active.as_deref(),
                )
            })
            .find(|e| e.spoken.trim().to_lowercase() == bk)
            .map(|e| e.id.clone());
        (format!("{} chars", row.spoken.chars().count()), dict_id)
    };
    match dict_id {
        Some(entry_id) => {
            // Delete fires delete→dismiss linkage: the suggestion row is
            // marked Dismissed, so the pair can never auto-re-accept.
            crate::dictionary::delete_dictionary_entry_internal(entry_id)
                .map_err(|e| format!("Failed to remove dictionary entry: {}", e))?;
            log::info!(
                "[AutoLearn] Reverted auto-added suggestion ({}), dictionary entry removed",
                spoken
            );
        }
        None => {
            // Dictionary row already gone (deleted elsewhere - linkage will
            // have dismissed it); ensure the veto stands.
            let _guard = SUGGESTION_LOCK.lock().map_err(|e| e.to_string())?;
            let mut database = load_from_disk().map_err(|e| e.to_string())?;
            if let Some(s) = database.suggestions.iter_mut().find(|s| s.id == id) {
                s.status = SuggestionStatus::Dismissed;
            }
            save_to_disk(&database).map_err(|e| e.to_string())?;
            log::info!(
                "[AutoLearn] Reverted auto-added suggestion ({}), no dictionary row remained",
                spoken
            );
        }
    }
    Ok(())
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

    fn pending_entry(spoken: &str, corrected: &str, frequency: u32) -> SuggestionEntry {
        SuggestionEntry {
            id: "test-id".to_string(),
            spoken: spoken.to_string(),
            corrected: corrected.to_string(),
            frequency,
            created_at: now_iso8601(),
            last_seen: now_iso8601(),
            source: SuggestionSource::Auto,
            status: SuggestionStatus::Pending,
            accepted_at: None,
        }
    }

    fn accepted_entry(spoken: &str, corrected: &str, accepted_at: Option<&str>) -> SuggestionEntry {
        SuggestionEntry {
            id: "test-id".to_string(),
            spoken: spoken.to_string(),
            corrected: corrected.to_string(),
            frequency: 3,
            created_at: now_iso8601(),
            last_seen: now_iso8601(),
            source: SuggestionSource::Auto,
            status: SuggestionStatus::Accepted,
            accepted_at: accepted_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_observed_same_day_is_false() {
        assert!(!observed_on_different_days(
            "2026-09-04T10:00:00+00:00",
            "2026-09-04T22:00:00+00:00"
        ));
    }

    #[test]
    fn test_observed_different_days_is_true() {
        assert!(observed_on_different_days(
            "2026-09-04T10:00:00+00:00",
            "2026-09-05T10:00:00+00:00"
        ));
    }

    #[test]
    fn test_observed_calendar_days_not_24h() {
        // 23:59 → 00:01 next day is 2 minutes but different calendar days.
        assert!(observed_on_different_days(
            "2026-09-04T23:59:00+00:00",
            "2026-09-05T00:01:00+00:00"
        ));
    }

    #[test]
    fn test_observed_unparseable_fails_closed() {
        assert!(!observed_on_different_days(
            "not-a-time",
            "2026-09-05T00:01:00+00:00"
        ));
        assert!(!observed_on_different_days(
            "2026-09-04T23:59:00+00:00",
            "not-a-time"
        ));
    }

    fn dict_entry(
        spoken: &str,
        deleted: bool,
        account: Option<&str>,
    ) -> crate::dictionary::DictionaryEntry {
        crate::dictionary::DictionaryEntry {
            spoken: spoken.to_string(),
            deleted_at: if deleted { Some(1) } else { None },
            sync_account: account.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_spoken_key_free() {
        let entries = vec![dict_entry("other", false, Some("a"))];
        assert_eq!(
            spoken_key_state(&entries, "shunade", Some("a")),
            SpokenKeyState::Free
        );
    }

    #[test]
    fn test_spoken_key_live_taken() {
        let entries = vec![dict_entry("Shunade", false, Some("a"))];
        assert_eq!(
            spoken_key_state(&entries, "shunade", Some("a")),
            SpokenKeyState::LiveTaken
        );
    }

    #[test]
    fn test_spoken_key_tombstoned() {
        // Live row absent, tombstone present → resurrecting would override
        // an explicit user delete.
        let entries = vec![dict_entry("shunade", true, Some("a"))];
        assert_eq!(
            spoken_key_state(&entries, "shunade", Some("a")),
            SpokenKeyState::Tombstoned
        );
    }

    #[test]
    fn test_spoken_key_live_beats_tombstone() {
        let entries = vec![
            dict_entry("shunade", true, Some("a")),
            dict_entry("shunade", false, Some("a")),
        ];
        assert_eq!(
            spoken_key_state(&entries, "shunade", Some("a")),
            SpokenKeyState::LiveTaken
        );
    }

    #[test]
    fn test_spoken_key_other_account_ignored() {
        let entries = vec![
            dict_entry("shunade", false, Some("b")),
            dict_entry("shunade", true, Some("b")),
        ];
        assert_eq!(
            spoken_key_state(&entries, "shunade", Some("a")),
            SpokenKeyState::Free
        );
    }

    #[test]
    fn test_spoken_key_normalization_matches_add() {
        // Same trim().to_lowercase() normalization as add_dictionary_entry.
        let entries = vec![dict_entry("  SHUNADE  ", false, Some("a"))];
        assert_eq!(
            spoken_key_state(&entries, "shunade", Some("a")),
            SpokenKeyState::LiveTaken
        );
    }

    #[test]
    fn test_first_sighting_stays_hidden() {
        // A one-off observation (typo, mid-typing state) records but never
        // surfaces - this is the Shunadi → Sined case.
        assert!(!is_ready_to_surface(&pending_entry("Shunadi", "Sined", 1)));
    }

    #[test]
    fn test_repeat_sighting_surfaces() {
        // The same pair seen twice is a systematic mishearing: surface it.
        assert!(is_ready_to_surface(&pending_entry("sined", "Sinead", 2)));
    }

    #[test]
    fn test_non_pending_never_surfaces() {
        // Dismissed and accepted rows stay buried no matter the frequency.
        let mut dismissed = pending_entry("or", "Message", 9);
        dismissed.status = SuggestionStatus::Dismissed;
        assert!(!is_ready_to_surface(&dismissed));
        let mut accepted = pending_entry("or", "Message", 9);
        accepted.status = SuggestionStatus::Accepted;
        assert!(!is_ready_to_surface(&accepted));
    }

    #[test]
    fn test_auto_added_feed_requires_stamp() {
        // Auto-accepted by the trigger: stamped → in provenance set.
        let mut auto = accepted_entry("sined", "Sinead", Some("2026-09-05T12:00:00+00:00"));
        assert!(is_auto_added(&auto));
        // Manually accepted Auto row (no stamp): excluded.
        auto.accepted_at = None;
        assert!(!is_auto_added(&auto));
        // Manual-source row with a stamp (shouldn't happen): excluded.
        let mut manual = accepted_entry("sined", "Sinead", Some("2026-09-05T12:00:00+00:00"));
        manual.source = SuggestionSource::Manual;
        assert!(!is_auto_added(&manual));
        // Pending rows never appear regardless of stamps.
        let mut pending = pending_entry("sined", "Sinead", 9);
        pending.accepted_at = Some("2026-09-05T12:00:00+00:00".to_string());
        assert!(!is_auto_added(&pending));
    }

    #[test]
    fn test_count_dismissed_counts_only_dismissed() {
        let db = SuggestionDatabase {
            version: 1,
            suggestions: vec![
                pending_entry("a", "b", 1),
                {
                    let mut s = pending_entry("c", "d", 3);
                    s.status = SuggestionStatus::Dismissed;
                    s
                },
                {
                    let mut s = pending_entry("e", "f", 2);
                    s.status = SuggestionStatus::Dismissed;
                    s
                },
                accepted_entry("g", "h", Some("2026-09-05T12:00:00+00:00")),
            ],
        };
        assert_eq!(count_dismissed(&db), 2);
        assert_eq!(count_dismissed(&SuggestionDatabase::default()), 0);
    }
}
