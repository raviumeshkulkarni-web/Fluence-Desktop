// Fluence Windows — Text Expansion (Snippets)
// Ported from the Android app: a spoken trigger phrase is replaced by
// expansion text in the FINAL transcript, after dictionary corrections.
//
// SECURITY INVARIANT: snippet data is applied only to the final
// transcript text. It NEVER enters the STT recognition prompt — the
// prompt is built exclusively from dictionary correction entries
// (see transcribe::build_vocabulary_hint).

use anyhow::Result;
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MAX_TRIGGER_LENGTH: usize = 100;
pub const MAX_EXPANSION_LENGTH: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: String,
    pub trigger: String,
    pub expansion: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default = "default_true_snip")]
    pub is_enabled: bool,
    #[serde(default)]
    pub deleted_at: Option<i64>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub ever_pushed: bool,
    #[serde(default)]
    pub sync_account: Option<String>,
    #[serde(default)]
    pub sync_state: Option<String>,
    #[serde(default)]
    pub server_file_id: Option<String>,
    #[serde(default)]
    pub quarantine_reason: Option<String>,
}

fn default_true_snip() -> bool {
    true
}

impl Default for Snippet {
    fn default() -> Self {
        Self {
            id: String::new(),
            trigger: String::new(),
            expansion: String::new(),
            created_at: None,
            updated_at: None,
            device_id: None,
            is_enabled: true,
            deleted_at: None,
            dirty: false,
            ever_pushed: false,
            sync_account: None,
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnippetStore {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}

impl Default for SnippetStore {
    fn default() -> Self {
        Self {
            enabled: false,
            snippets: Vec::new(),
        }
    }
}

static STORE_CACHE: Mutex<Option<SnippetStore>> = Mutex::new(None);

fn snippets_path() -> PathBuf {
    let mut path = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("snippets.json");
    path
}

pub(crate) fn load_store_internal() -> Result<SnippetStore> {
    let path = snippets_path();
    if !path.exists() {
        return Ok(SnippetStore::default());
    }
    let data = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data).unwrap_or_default())
}

pub(crate) fn save_store_internal(store: &SnippetStore) -> Result<()> {
    let path = snippets_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &data)?;
    if let Ok(f) = fs::File::open(&tmp_path) {
        let _ = f.sync_all();
    }
    fs::rename(&tmp_path, &path)?;
    if let Ok(f) = fs::File::open(&path) {
        let _ = f.sync_all();
    }
    // Sync imports write through this function too. Drop the runtime cache so
    // the next transcription sees newly merged expansions immediately.
    invalidate_cache();
    Ok(())
}

fn cached_store() -> SnippetStore {
    let cache = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(store) = cache.as_ref() {
        return store.clone();
    }
    drop(cache);
    let store = load_store_internal().unwrap_or_default();
    let mut cache2 = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache2 = Some(store.clone());
    store
}

pub(crate) fn invalidate_cache() {
    let mut cache = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache = None;
}

/// Post-transcription pipeline: dictionary corrections first, then
/// snippet expansion (only when the master toggle is enabled).
/// This runs on the final text only — never on the STT prompt.
pub fn process_transcript(text: &str) -> String {
    let corrected = crate::dictionary::apply_corrections(text);
    let store = cached_store();
    if !store.enabled {
        return corrected;
    }
    let active_account = crate::sync::metadata::current_account_hash();
    let live: Vec<Snippet> = store
        .snippets
        .into_iter()
        .filter(|s| s.deleted_at.is_none() && s.is_enabled) // deleted never expand, disabled never expand
        .filter(|s| {
            crate::sync::metadata::belongs_to_account(
                s.sync_account.as_deref(),
                active_account.as_deref(),
            )
        })
        .collect();
    expand_with(&corrected, &live)
}

fn fold_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_word_boundary(chars: &[char], start: usize, end: usize) -> bool {
    let before_ok = start == 0 || !chars[start - 1].is_alphanumeric();
    let after_ok = end >= chars.len() || !chars[end].is_alphanumeric();
    before_ok && after_ok
}

/// Deterministic text-in/text-out expansion of the final transcript.
///
/// Matches the Android SnippetProcessor semantics:
/// - exact phrase matching, case-insensitive via an unconditional
///   (locale-independent) case fold on both sides,
/// - word/phrase boundaries (the character before/after the trigger must
///   be absent or a non-letter/digit; punctuation is a valid boundary),
/// - when several snippets match at the same position the longest trigger
///   wins,
/// - all non-overlapping occurrences in one pass are replaced,
/// - expansion text is never re-scanned (no cascading expansion).
///
/// Invalid snippets (blank trigger or expansion) are skipped silently.
pub fn expand_with(raw_text: &str, snippets: &[Snippet]) -> String {
    if raw_text.trim().is_empty() || snippets.is_empty() {
        return raw_text.to_string();
    }

    #[derive(Clone, Copy)]
    struct MatchSpan {
        snippet: usize,
        start: usize,
        end: usize,
    }

    let chars: Vec<char> = raw_text.chars().collect();
    let mut matches: Vec<MatchSpan> = Vec::new();

    for (snippet_idx, snippet) in snippets.iter().enumerate() {
        let trigger: Vec<char> = snippet.trigger.trim().chars().collect();
        if trigger.is_empty() || snippet.expansion.trim().is_empty() {
            continue;
        }
        let mut i = 0usize;
        while i < chars.len() {
            if fold_char(chars[i]) == fold_char(trigger[0]) {
                let mut j = 0usize;
                let mut matched = true;
                while j < trigger.len() {
                    if i + j >= chars.len() || fold_char(chars[i + j]) != fold_char(trigger[j]) {
                        matched = false;
                        break;
                    }
                    j += 1;
                }
                if matched && is_word_boundary(&chars, i, i + trigger.len()) {
                    matches.push(MatchSpan {
                        snippet: snippet_idx,
                        start: i,
                        end: i + trigger.len(),
                    });
                    i += trigger.len();
                    continue;
                }
            }
            i += 1;
        }
    }

    if matches.is_empty() {
        return raw_text.to_string();
    }

    // Longest trigger wins at each position, then keep non-overlapping
    // matches greedily (also drops the shorter sibling sharing a start).
    matches.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| b.end.cmp(&a.end)));
    let mut chosen: Vec<MatchSpan> = Vec::new();
    let mut last_end = 0usize;
    for m in matches {
        if m.start >= last_end {
            chosen.push(m);
            last_end = m.end;
        }
    }
    if chosen.is_empty() {
        return raw_text.to_string();
    }

    // Left-to-right build from the sorted, non-overlapping spans.
    // Expansion text is inserted verbatim and never re-scanned.
    let mut out = String::with_capacity(raw_text.len());
    let mut cursor = 0usize;
    for m in &chosen {
        for &c in &chars[cursor..m.start] {
            out.push(c);
        }
        out.push_str(&snippets[m.snippet].expansion);
        cursor = m.end;
    }
    for &c in &chars[cursor..] {
        out.push(c);
    }
    out
}

fn validate_snippet(trigger: &str, expansion: &str) -> Result<(String, String), String> {
    let trigger = trigger.trim();
    let expansion = expansion.trim();
    if trigger.is_empty() || expansion.is_empty() {
        return Err("Trigger and expansion text must not be empty".to_string());
    }
    if trigger.chars().count() > MAX_TRIGGER_LENGTH
        || expansion.chars().count() > MAX_EXPANSION_LENGTH
    {
        return Err(format!(
            "Trigger must be at most {} characters and expansion at most {} characters",
            MAX_TRIGGER_LENGTH, MAX_EXPANSION_LENGTH
        ));
    }
    Ok((trigger.to_string(), expansion.to_string()))
}

fn trigger_collides(snippets: &[Snippet], id: &str, trigger: &str) -> bool {
    snippets
        .iter()
        .any(|s| s.id != id && s.trigger.to_lowercase() == trigger.to_lowercase())
}

// Tauri Commands

#[tauri::command]
pub fn get_snippets() -> Result<SnippetStore, String> {
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    let active = crate::sync::metadata::current_account_hash();
    store.snippets.retain(|s| {
        s.deleted_at.is_none()
            && crate::sync::metadata::belongs_to_account(
                s.sync_account.as_deref(),
                active.as_deref(),
            )
    }); // live current-account view only
    Ok(store)
}

#[tauri::command]
pub fn set_snippets_enabled(
    enabled: bool,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    store.enabled = enabled;
    save_store_internal(&store).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn add_snippet(
    trigger: String,
    expansion: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<Snippet, String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let (trigger, expansion) = validate_snippet(&trigger, &expansion)?;
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    let active_account = crate::sync::metadata::current_account_hash();
    let live: Vec<Snippet> = store
        .snippets
        .iter()
        .filter(|s| {
            s.deleted_at.is_none()
                && crate::sync::metadata::belongs_to_account(
                    s.sync_account.as_deref(),
                    active_account.as_deref(),
                )
        })
        .cloned()
        .collect();
    if trigger_collides(&live, "", &trigger) {
        return Err("A snippet with this trigger already exists".to_string());
    }
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
    if let Some(h) = account_hash.as_deref() {
        meta.update_max_seen(h, new_max);
    }
    let snippet = Snippet {
        id: uuid::Uuid::new_v4().to_string(),
        trigger,
        expansion,
        created_at: Some(now),
        updated_at: Some(now),
        device_id: Some(device_id),
        is_enabled: true,
        deleted_at: None,
        dirty: true,
        ever_pushed: false,
        sync_account: account_hash,
        sync_state: None,
        server_file_id: None,
        quarantine_reason: None,
    };
    store.snippets.push(snippet.clone());
    save_store_internal(&store).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(snippet)
}

#[tauri::command]
pub fn update_snippet(
    id: String,
    trigger: String,
    expansion: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let (trigger, expansion) = validate_snippet(&trigger, &expansion)?;
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    let active_account = crate::sync::metadata::current_account_hash();
    let live: Vec<Snippet> = store
        .snippets
        .iter()
        .filter(|s| {
            s.deleted_at.is_none()
                && crate::sync::metadata::belongs_to_account(
                    s.sync_account.as_deref(),
                    active_account.as_deref(),
                )
        })
        .cloned()
        .collect();
    if trigger_collides(&live, &id, &trigger) {
        return Err("A snippet with this trigger already exists".to_string());
    }
    // Frozen v1.1: same syncId on edit
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
    let mut found = false;
    for s in store.snippets.iter_mut() {
        if s.id == id {
            if !crate::sync::metadata::belongs_to_account(
                s.sync_account.as_deref(),
                active_account.as_deref(),
            ) {
                return Err("Snippet belongs to another account".to_string());
            }
            if s.deleted_at.is_some() {
                return Err("Cannot edit deleted snippet".to_string());
            }
            found = true;
            s.trigger = trigger.clone();
            s.expansion = expansion.clone();
            s.updated_at = Some(now);
            s.device_id = Some(device_id.clone());
            s.dirty = true;
            break;
        }
    }
    if !found {
        return Err("Snippet not found".to_string());
    }
    save_store_internal(&store).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn delete_snippet(
    id: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    let _io = crate::sync::io_lock::io_lock_guard();
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
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
    for s in store.snippets.iter_mut() {
        if s.id == id {
            if !crate::sync::metadata::belongs_to_account(
                s.sync_account.as_deref(),
                active_account.as_deref(),
            ) {
                return Err("Snippet belongs to another account".to_string());
            }
            if !s.ever_pushed {
                to_hard_delete = true;
            } else {
                s.deleted_at = Some(now);
                s.updated_at = Some(now);
                s.device_id = Some(device_id.clone());
                s.dirty = true;
            }
        }
    }
    if to_hard_delete {
        store.snippets.retain(|s| s.id != id);
    }
    save_store_internal(&store).map_err(|e| e.to_string())?;
    scheduler.command(crate::sync::scheduler::SyncCommand::LocalChange);
    invalidate_cache();
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync note (frozen v1.2): the sync-facing store lives in
// `crate::sync::stores::SnippetDirtyStore`. History never syncs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(id: &str, trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: id.to_string(),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn deleted_snippets_never_expand() {
        let mut s = snippet("1", "ping", "pong");
        s.deleted_at = Some(1713462000456);
        let live: Vec<Snippet> = vec![s.clone()]
            .into_iter()
            .filter(|s| s.deleted_at.is_none())
            .collect();
        assert_eq!(expand_with("ping", &live), "ping");
    }

    #[test]
    fn case_insensitive_matching() {
        let snippets = vec![snippet("1", "my linkedin", "url")];
        assert_eq!(expand_with("MY LINKEDIN", &snippets), "url");
        assert_eq!(expand_with("My LinkedIn", &snippets), "url");
        assert_eq!(expand_with("my linkedin", &snippets), "url");
    }

    #[test]
    fn punctuation_is_a_valid_boundary_and_is_preserved() {
        let snippets = vec![snippet("1", "my linkedin", "url")];
        assert_eq!(expand_with("check my linkedin", &snippets), "check url");
        assert_eq!(expand_with("(my linkedin) ok", &snippets), "(url) ok");
        assert_eq!(expand_with("send my linkedin!", &snippets), "send url!");
        assert_eq!(expand_with("the my linkedin.", &snippets), "the url.");
        assert_eq!(expand_with("\"my linkedin\"", &snippets), "\"url\"");
        assert_eq!(expand_with("  my linkedin  ", &snippets), "  url  ");
    }

    #[test]
    fn word_boundary_prevents_partial_word_matches() {
        let snippets = vec![snippet("1", "cat", "X")];
        assert_eq!(expand_with("abc", &snippets), "abc");
        assert_eq!(
            expand_with("The cat is in the concatenate category.", &snippets),
            "The X is in the concatenate category."
        );
        let linkedin = vec![snippet("1", "linkedin", "url")];
        assert_eq!(expand_with("linkedins", &linkedin), "linkedins");
    }

    #[test]
    fn all_non_overlapping_occurrences_replaced() {
        let snippets = vec![snippet("1", "my linkedin", "url")];
        assert_eq!(
            expand_with("my linkedin and my linkedin", &snippets),
            "url and url"
        );
    }

    #[test]
    fn longest_trigger_wins_at_same_position() {
        let snippets = vec![
            snippet("1", "my linkedin", "short"),
            snippet("2", "my linkedin profile", "long"),
        ];
        assert_eq!(
            expand_with("my linkedin profile is mine", &snippets),
            "long is mine"
        );
    }

    #[test]
    fn unicode_triggers_and_boundaries() {
        let snippets = vec![snippet("1", "извините", "sorry")];
        assert_eq!(
            expand_with("Извините, пожалуйста", &snippets),
            "sorry, пожалуйста"
        );
        assert_eq!(expand_with("ИЗВИНИТЕ", &snippets), "sorry");

        let emoji = vec![snippet("1", "my linkedin", "url")];
        assert_eq!(expand_with("\u{1F44D}my linkedin", &emoji), "\u{1F44D}url");
        assert_eq!(expand_with("my linkedin\u{1F44D}", &emoji), "url\u{1F44D}");
    }

    #[test]
    fn expansion_text_is_never_rescanned() {
        let snippets = vec![snippet("1", "ping", "pong"), snippet("2", "pong", "pang")];
        assert_eq!(expand_with("ping", &snippets), "pong");
        assert_eq!(expand_with("pong", &snippets), "pang");
    }

    #[test]
    fn blank_text_or_empty_snippets_are_unchanged() {
        let snippets = vec![snippet("1", "a", "b")];
        assert_eq!(expand_with("", &snippets), "");
        assert_eq!(expand_with("hello", &[]), "hello");
        assert_eq!(expand_with("   ", &snippets), "   ");
    }

    #[test]
    fn invalid_snippets_are_skipped_silently() {
        let snippets = vec![
            snippet("1", "   ", "x"),
            snippet("2", "ok", "   "),
            snippet("3", "valid", "fine"),
        ];
        assert_eq!(expand_with("valid ok", &snippets), "fine ok");
    }

    #[test]
    fn validate_rejects_blank_and_oversized() {
        assert!(validate_snippet("  ", "x").is_err());
        assert!(validate_snippet("x", "").is_err());
        let long_trigger = "a".repeat(MAX_TRIGGER_LENGTH + 1);
        assert!(validate_snippet(&long_trigger, "x").is_err());
        let long_expansion = "a".repeat(MAX_EXPANSION_LENGTH + 1);
        assert!(validate_snippet("x", &long_expansion).is_err());
        assert!(validate_snippet("x", "y").is_ok());
    }

    #[test]
    fn trigger_collision_is_case_insensitive() {
        let snippets = vec![snippet("1", "My LinkedIn", "url")];
        assert!(trigger_collides(&snippets, "", "my linkedin"));
        assert!(trigger_collides(&snippets, "2", "MY LINKEDIN"));
        assert!(!trigger_collides(&snippets, "1", "my linkedin"));
        assert!(!trigger_collides(&snippets, "", "other"));
    }
}
