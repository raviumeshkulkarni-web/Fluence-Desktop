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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snippet {
    pub id: String,
    pub trigger: String,
    pub expansion: String,
    // §30 sync metadata (see dictionary.rs — same contract).
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

fn load_store_internal() -> Result<SnippetStore> {
    let path = snippets_path();
    if !path.exists() {
        return Ok(SnippetStore::default());
    }
    let data = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data).unwrap_or_default())
}

fn save_store_internal(store: &SnippetStore) -> Result<()> {
    let path = snippets_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    fs::write(&path, data)?;
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

fn invalidate_cache() {
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
    let live: Vec<Snippet> = store
        .snippets
        .into_iter()
        .filter(|s| s.deleted_at.is_none()) // §30.2: deleted snippets never expand
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
    store.snippets.retain(|s| s.deleted_at.is_none()); // live view only
    Ok(store)
}

#[tauri::command]
pub fn set_snippets_enabled(enabled: bool) -> Result<(), String> {
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    store.enabled = enabled;
    save_store_internal(&store).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn add_snippet(trigger: String, expansion: String) -> Result<Snippet, String> {
    let (trigger, expansion) = validate_snippet(&trigger, &expansion)?;
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    let live: Vec<Snippet> = store
        .snippets
        .iter()
        .filter(|s| s.deleted_at.is_none())
        .cloned()
        .collect();
    if trigger_collides(&live, "", &trigger) {
        return Err("A snippet with this trigger already exists".to_string());
    }
    let snippet = Snippet {
        id: uuid::Uuid::new_v4().to_string(),
        trigger,
        expansion,
        created_at: Some(chrono::Utc::now().timestamp_millis()),
        deleted_at: None,
        sync_state: None,
        server_file_id: None,
        sync_account: None,
        quarantine_reason: None,
    };
    store.snippets.push(snippet.clone());
    save_store_internal(&store).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(snippet)
}

#[tauri::command]
pub fn update_snippet(id: String, trigger: String, expansion: String) -> Result<(), String> {
    let (trigger, expansion) = validate_snippet(&trigger, &expansion)?;
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    let live: Vec<Snippet> = store
        .snippets
        .iter()
        .filter(|s| s.deleted_at.is_none())
        .cloned()
        .collect();
    if trigger_collides(&live, &id, &trigger) {
        return Err("A snippet with this trigger already exists".to_string());
    }
    // §30.2: an edit is a tombstone + a new UUID (see dictionary.rs).
    let now = chrono::Utc::now().timestamp_millis();
    let mut found = false;
    for s in store.snippets.iter_mut() {
        if s.id == id {
            found = true;
            s.deleted_at = Some(now);
            if s.server_file_id.is_some() {
                s.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
            }
            break;
        }
    }
    if !found {
        return Err("Snippet not found".to_string());
    }
    store.snippets.push(Snippet {
        id: uuid::Uuid::new_v4().to_string(),
        trigger,
        expansion,
        created_at: Some(now),
        deleted_at: None,
        sync_state: None,
        server_file_id: None,
        sync_account: None,
        quarantine_reason: None,
    });
    save_store_internal(&store).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(())
}

#[tauri::command]
pub fn delete_snippet(id: String) -> Result<(), String> {
    let mut store = load_store_internal().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut removed = false;
    for s in store.snippets.iter_mut() {
        if s.id == id {
            if s.server_file_id.is_some() {
                // Uploaded → tombstone so other devices delete it too (§30.2).
                s.deleted_at = Some(now);
                s.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
            } else {
                // Never uploaded → provably safe to hard-delete (§14).
                removed = true;
            }
        }
    }
    if removed {
        store.snippets.retain(|s| s.id != id);
    }
    save_store_internal(&store).map_err(|e| e.to_string())?;
    invalidate_cache();
    Ok(())
}

// ---------------------------------------------------------------------------
// §30 sync — SnippetSyncStore (LocalStore seam for the snippet kind).
// Wired into the desktop binary by the Phase 7 scheduler.
// ---------------------------------------------------------------------------

pub(crate) mod sync_store {
    use super::*;

    use crate::sync::engine::{
        LocalRow, LocalStore, QuarantineReason, SyncError, SYNC_STATE_LOCAL,
    };
    use crate::sync::wire::RecordType;

    /// Sync-facing seam over the same persisted store (§30.2). Keeps every row —
    /// live, tombstoned, latched — so the engine can reconcile them all; user
    /// reads (`get_snippets`, expansion) see only live rows.
    #[derive(Debug, Default)]
    pub struct SnippetSyncStore {
        pub store: SnippetStore,
    }

    impl SnippetSyncStore {
        pub fn new() -> Self {
            let mut sync_store = Self {
                store: load_store_internal().unwrap_or_default(),
            };
            sync_store.backfill_legacy_created_at();
            sync_store
        }

        #[cfg(test)]
        pub fn snippets(&self) -> &[Snippet] {
            &self.store.snippets
        }

        pub fn backfill_legacy_created_at(&mut self) {
            let now = chrono::Utc::now().timestamp_millis();
            let mut changed = false;
            for s in self.store.snippets.iter_mut() {
                if s.created_at.is_none() {
                    s.created_at = Some(now);
                    changed = true;
                }
            }
            if changed {
                self.save();
            }
        }

        fn save(&self) {
            if save_store_internal(&self.store).is_ok() {
                invalidate_cache();
            }
        }
    }

    impl LocalStore for SnippetSyncStore {
        fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow> {
            let mut out: Vec<LocalRow> = self
                .store
                .snippets
                .iter()
                .filter(|s| match account {
                    None => s.sync_account.is_none(),
                    Some(a) => s.sync_account.as_deref().map_or(true, |s| s == a),
                })
                .map(snippet_to_local)
                .collect();
            out.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            out
        }

        fn find_row(&self, uuid: &str) -> Option<LocalRow> {
            self.store
                .snippets
                .iter()
                .find(|s| s.id == uuid)
                .map(snippet_to_local)
        }

        fn import(&mut self, row: LocalRow) -> Result<(), SyncError> {
            let Some(snippet) = local_to_snippet(row) else {
                return Ok(()); // other kinds never reach this store
            };
            if let Some(existing) = self.store.snippets.iter_mut().find(|s| s.id == snippet.id) {
                *existing = snippet;
            } else {
                self.store.snippets.push(snippet);
            }
            self.save();
            Ok(())
        }

        fn mark_tombstoned(&mut self, uuid: &str, deleted_at: i64) -> Result<(), SyncError> {
            if let Some(s) = self.store.snippets.iter_mut().find(|s| s.id == uuid) {
                s.deleted_at = Some(deleted_at);
                s.sync_state = Some(crate::sync::engine::SYNC_STATE_DIRTY.to_string());
            }
            self.save();
            Ok(())
        }

        fn set_server_file_id(&mut self, uuid: &str, file_id: &str) -> Result<(), SyncError> {
            if let Some(s) = self.store.snippets.iter_mut().find(|s| s.id == uuid) {
                s.server_file_id = Some(file_id.to_string());
            }
            self.save();
            Ok(())
        }

        fn set_sync_state(&mut self, uuid: &str, state: &str) -> Result<(), SyncError> {
            if let Some(s) = self.store.snippets.iter_mut().find(|s| s.id == uuid) {
                s.sync_state = Some(state.to_string());
            }
            self.save();
            Ok(())
        }

        fn quarantine(&mut self, uuid: &str, reason: QuarantineReason) -> Result<(), SyncError> {
            if let Some(s) = self.store.snippets.iter_mut().find(|s| s.id == uuid) {
                s.quarantine_reason = Some(reason.as_str().to_string());
                s.sync_state = Some(crate::sync::engine::SYNC_STATE_QUARANTINED.to_string());
            }
            self.save();
            Ok(())
        }

        fn clear_quarantine(&mut self, uuid: &str) -> Result<(), SyncError> {
            if let Some(s) = self.store.snippets.iter_mut().find(|s| s.id == uuid) {
                s.quarantine_reason = None;
                s.sync_state = Some(SYNC_STATE_LOCAL.to_string());
            }
            self.save();
            Ok(())
        }

        fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError> {
            self.store.snippets.retain(|s| s.id != uuid);
            self.save();
            Ok(())
        }
    }

    fn snippet_to_local(s: &Snippet) -> LocalRow {
        LocalRow {
            uuid: s.id.clone(),
            timestamp_ms: s.created_at.unwrap_or(0),
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Snippet,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: Some(s.trigger.clone()),
            expansion: Some(s.expansion.clone()),
            settings_key: None,
            settings_value: None,
            deleted_at: s.deleted_at,
            server_file_id: s.server_file_id.clone(),
            sync_account: s.sync_account.clone(),
            sync_state: s
                .sync_state
                .clone()
                .unwrap_or_else(|| SYNC_STATE_LOCAL.to_string()),
            quarantine_reason: s.quarantine_reason.clone(),
        }
    }

    fn local_to_snippet(row: LocalRow) -> Option<Snippet> {
        if row.rtype != RecordType::Snippet {
            return None;
        }
        Some(Snippet {
            id: row.uuid,
            trigger: row.trigger.unwrap_or_default(),
            expansion: row.expansion.unwrap_or_default(),
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

    fn snippet(id: &str, trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: id.to_string(),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn sync_store_roundtrip_through_wire() {
        const U: &str = "00000000-0000-4000-8000-000000000006";
        let mut store = SnippetSyncStore::default();
        store
            .import(local_to_snippet_live(U, "addr", "456 Oak Ave"))
            .unwrap();
        let row = store.find_row(U).unwrap();
        let rec = crate::sync::wire::parse(row.to_wire().to_json().as_bytes(), U).unwrap();
        assert_eq!(rec.rtype, RecordType::Snippet);
        assert_eq!(rec.trigger.as_deref(), Some("addr"));
        assert_eq!(rec.expansion.as_deref(), Some("456 Oak Ave"));
        assert_eq!(
            store.find_row(U).unwrap().to_wire().to_json(),
            row.to_wire().to_json()
        );
    }

    fn local_to_snippet_live(uuid: &str, trigger: &str, expansion: &str) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713468000123,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Snippet,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: Some(trigger.to_string()),
            expansion: Some(expansion.to_string()),
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
    fn sync_store_backfills_legacy_snippets() {
        let mut store = SnippetSyncStore::default();
        let legacy = snippet("legacy", "addr", "1 Example St");
        let mut legacy = legacy;
        legacy.created_at = None;
        store.store.snippets.push(legacy);
        store.backfill_legacy_created_at();
        assert!(store.store.snippets[0].created_at.is_some());
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
