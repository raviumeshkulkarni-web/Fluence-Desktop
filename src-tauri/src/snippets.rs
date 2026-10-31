// Fluence Windows - Text Expansion (Snippets)
// Ported from the Android app: a spoken trigger phrase is replaced by
// expansion text in the FINAL transcript, after dictionary corrections.
//
// SECURITY INVARIANT: snippet data is applied only to the final
// transcript text. It NEVER enters the STT recognition prompt - the
// prompt is built exclusively from dictionary correction entries
// (see transcribe::build_vocabulary_hint).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use unicode_normalization::UnicodeNormalization;

pub const MAX_TRIGGER_LENGTH: usize = 100;
pub const MAX_EXPANSION_LENGTH: usize = 500;

/// Sync business key of a trigger — the exact identity
/// [`crate::sync::domain::SnippetItem::business_key`] derives (NFC +
/// lowercase + trim), so store-level key matching agrees with what the merge
/// treats as one record (H4 rescue).
pub(crate) fn snippet_sync_business_key(trigger: &str) -> String {
    trigger.trim().nfc().collect::<String>().to_lowercase()
}

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

/// Uses the shared `sync::stores::base_data_dir()` so test builds can never
/// write the real snippets file. Unset in production, so the path is unchanged.
fn snippets_path() -> PathBuf {
    let mut path = crate::sync::stores::base_data_dir();
    path.push("Fluence");
    path.push("snippets.json");
    path
}

pub(crate) fn load_store_internal() -> Result<SnippetStore> {
    load_store_file(&snippets_path())
}

/// Path-parameterized core of [`load_store_internal`]. Tests drive this with
/// unique temp paths so corrupt-file recovery is verified hermetically,
/// never against the shared test data root.
pub(crate) fn load_store_file(path: &std::path::Path) -> Result<SnippetStore> {
    match crate::sync::recover::read_json_file(path) {
        // Missing file is a fresh store, not corruption: no backup, writes
        // proceed normally.
        Ok(None) => Ok(SnippetStore::default()),
        Ok(Some(store)) => Ok(store),
        // Corrupt bytes are preserved aside inside `read_json_file`; surface
        // the error so write paths fail closed instead of persisting an
        // empty store over unreadable state (Phase E).
        Err(e) => Err(anyhow::anyhow!("unreadable snippets store: {e:?}")),
    }
}

pub(crate) fn save_store_internal(store: &SnippetStore) -> Result<()> {
    let path = snippets_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    // The tmp name MUST be unique per caller. This used to be the fixed
    // `path.with_extension("json.tmp")`, the exact bug `settings.rs:371-379`
    // documents and `sync/stores.rs` + `account_scope.rs` already fixed: two
    // concurrent writers both wrote the SAME tmp, then both renamed it, so the
    // loser either renamed a tmp the winner had already consumed (os error 2) or
    // renamed a partially-written tmp onto the destination — truncating a live
    // store. Audit #22 fixed the two files it happened to touch and left these
    // two. pid + monotonic sequence, in the destination's own directory so the
    // rename stays same-volume atomic.
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
    // Sync imports write through this function too. Drop the runtime cache so
    // the next transcription sees newly merged expansions immediately.
    invalidate_cache();
    Ok(())
}

/// Observable form of [`cached_store`] (audit Round-4 F3, revalidated
/// Round-4 second pass): identical cache semantics - including caching the
/// degraded default on unreadable bytes, so the read failure surfaces
/// EXACTLY ONCE per cache generation instead of warning on every dictation
/// take - and the `warn` is recorded HERE, at the one point where the
/// degraded default is cached. Call order can never suppress it: whether
/// the first cache-miss caller is the expansion take (`cached_store`, which
/// discards the Err) or the echo shield, the branch fires before the value
/// is cached, so later cache-hit callers see Ok(default) after exactly one
/// warn.
fn cached_store_checked() -> Result<SnippetStore, anyhow::Error> {
    let cache = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(store) = cache.as_ref() {
        return Ok(store.clone());
    }
    drop(cache);
    // Deliberate, bounded degradation - NOT a Phase E oversight.
    //
    // `load_store_internal` fails only on unreadable bytes. On this path that
    // result is intentionally degraded and treated as "no expansions",
    // because these are READ-ONLY paths (expansion + the echo shield)
    // reached from `process_transcript`/`upsert_suggestions`: the transcript
    // has already been produced, there is no caller left to report an
    // error to, and failing would throw away a successful dictation for
    // something cosmetic. An empty store is the correct outcome of a
    // store that cannot be read as expansions.
    //
    // It is safe precisely because nothing here writes. The corruption is
    // preserved aside by `read_json_file`, and every *mutating* caller
    // (add/update/remove/import, the ones further down this file) still
    // resolves the store with `load_store_internal().map_err(..)?` and fails
    // closed, so a corrupt store can never be silently persisted over empty.
    // The two rules have to hold together: read degrades, write refuses.
    let (store, err) = match load_store_internal() {
        Ok(store) => (store, None),
        // The warning MUST live at this load point, not in the shield
        // wrapper: `cached_store` swallows the Err by design, so a wrapper
        // downstream of cached_store never sees it (the cache-first
        // ordering gap the Round-4 revalidation flagged). Logging here
        // records the degradation for BOTH read-only consumers, exactly
        // once per cache generation.
        Err(err) => {
            log::warn!(
                "[Snippets] Snippets store unreadable ({err:?}) - read-only paths run degraded: no expansions and corrections UNSHIELDED until the store is readable"
            );
            (SnippetStore::default(), Some(err))
        }
    };
    let mut cache2 = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache2 = Some(store.clone());
    match err {
        Some(err) => Err(err),
        None => Ok(store),
    }
}

fn cached_store() -> SnippetStore {
    // Clippy-clean form of: Ok(store) => store, Err(_) => SnippetStore::default().
    cached_store_checked().unwrap_or_default()
}

pub(crate) fn invalidate_cache() {
    let mut cache = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache = None;
}

/// Canonical trigger→expansion pair keys for ACTIVE snippets - the echo
/// shield the suggestion store uses (plan plan-2, Task 6).
///
/// A snippet whose expansion is a single word (e.g. "again" → "agent")
/// would otherwise have its own application re-learned as a correction
/// pair on every dictation take (+1 frequency per run, and the store's
/// users reported such rows at 8x). The expansion pipeline applies only
/// the same filters used for actual expansion (`process_transcript`):
/// master toggle on, not deleted, per-snippet enabled, account-scoped,
/// non-blank trigger/expansion.
///
/// Fail-open is a logged degradation, not a silent one (audit Round-4
/// F3, revalidated): `cached_store_checked` degrades to an empty store on
/// read errors and records the `warn` at that load point - an unreadable
/// snippet store can never block learning, and the lost shielding surfaces
/// as exactly one `warn` per cache generation regardless of which
/// read-only caller populates the cache first.
pub(crate) fn active_snippet_expansion_keys() -> std::collections::BTreeSet<String> {
    let active_account = crate::sync::metadata::current_account_hash();
    shield_keys_for(cached_store_checked(), active_account.as_deref())
}

/// Pure seam behind [`active_snippet_expansion_keys`]: the load result
/// is an argument, so the fail-open decision (error input → empty key
/// set; ok input → normal filter path) is hermetically testable without
/// touching the shared cfg(test) snippet store (the exact shared-root zone
/// audit F7 flagged). It logs NOTHING: the unreadable-store warn lives in
/// `cached_store_checked`'s error branch (Round-4 second pass) so the
/// record cannot depend on call order; an Err input here is the
/// already-recorded case.
fn shield_keys_for(
    load: Result<SnippetStore, anyhow::Error>,
    active_account: Option<&str>,
) -> std::collections::BTreeSet<String> {
    let store = load.unwrap_or_default();
    active_snippet_expansion_keys_from(&store, active_account)
}

/// Pure core of [`active_snippet_expansion_keys`]: hermetic and unit-testable.
pub(crate) fn active_snippet_expansion_keys_from(
    store: &SnippetStore,
    active_account: Option<&str>,
) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    // Master toggle off → no expansion is happening → no echo to shield.
    if !store.enabled {
        return keys;
    }
    for snippet in &store.snippets {
        if snippet.deleted_at.is_some() || !snippet.is_enabled {
            continue;
        }
        let trigger = snippet.trigger.trim();
        let expansion = snippet.expansion.trim();
        if trigger.is_empty() || expansion.is_empty() {
            continue;
        }
        if !crate::sync::metadata::belongs_to_account(
            snippet.sync_account.as_deref(),
            active_account,
        ) {
            continue;
        }
        // Single canonical key path shared with the dictionary/suggestion
        // stores, so the shield matches exactly what upsert compares.
        keys.insert(crate::dictionary::canonical_entry_key(trigger, expansion));
    }
    keys
}

/// Post-transcription pipeline: dictionary corrections first, then
/// snippet expansion (only when the master toggle is enabled).
/// This runs on the final text only - never on the STT prompt.
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
pub fn get_snippets(window: tauri::Window) -> Result<SnippetStore, String> {
    // Snippet content is user-authored text expansion data: main-only.
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
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
    window: tauri::Window,
    enabled: bool,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
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
    window: tauri::Window,
    trigger: String,
    expansion: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<Snippet, String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
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
    window: tauri::Window,
    id: String,
    trigger: String,
    expansion: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
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
    window: tauri::Window,
    id: String,
    scheduler: tauri::State<'_, crate::sync::scheduler::Scheduler>,
) -> Result<(), String> {
    crate::acl::require_caller(&window, &[crate::acl::MAIN_WINDOW])?;
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

    // ── plan plan-2 Task 6: active snippet expansion keys (echo shield) ──

    fn shield_store(enabled: bool, snippets: Vec<Snippet>) -> SnippetStore {
        SnippetStore { enabled, snippets }
    }

    #[test]
    fn test_active_snippet_keys_canonical_include_active_snippets() {
        let keys = active_snippet_expansion_keys_from(
            &shield_store(true, vec![snippet("1", "again", "agent")]),
            None,
        );
        assert_eq!(
            keys.iter().collect::<Vec<_>>(),
            vec![&crate::dictionary::canonical_entry_key("again", "agent")]
        );
    }

    #[test]
    fn test_active_snippet_keys_master_toggle_off_is_empty() {
        // Master toggle off → expansion pipeline never runs → no echo.
        let keys = active_snippet_expansion_keys_from(
            &shield_store(false, vec![snippet("1", "again", "agent")]),
            None,
        );
        assert!(keys.is_empty());
    }

    #[test]
    fn test_active_snippet_keys_excludes_disabled_and_deleted_and_blank() {
        let disabled_row = Snippet {
            is_enabled: false,
            ..snippet("2", "brb", "be right back")
        };
        let deleted_row = Snippet {
            deleted_at: Some(1700000000),
            ..snippet("3", "gtg", "got to go")
        };
        let blank_trigger = snippet("4", "   ", "x");
        let blank_expansion = snippet("5", "x", "  ");
        let keys = active_snippet_expansion_keys_from(
            &shield_store(
                true,
                vec![
                    disabled_row,
                    deleted_row,
                    blank_trigger,
                    blank_expansion,
                    snippet("6", "haha", "lol"),
                ],
            ),
            None,
        );
        assert_eq!(
            keys.iter().collect::<Vec<_>>(),
            vec![&crate::dictionary::canonical_entry_key("haha", "lol")]
        );
    }

    #[test]
    fn test_active_snippet_keys_account_scoped() {
        let other = Snippet {
            sync_account: Some("hash-b".to_string()),
            ..snippet("7", "brb", "be right back")
        };
        // Row without account matches any active account.
        let keys_any = active_snippet_expansion_keys_from(
            &shield_store(true, vec![snippet("1", "again", "agent")]),
            Some("hash-a"),
        );
        assert_eq!(keys_any.len(), 1);
        // Row with a different account hash: excluded when active is set…
        assert!(active_snippet_expansion_keys_from(
            &shield_store(true, vec![other.clone()]),
            Some("hash-a")
        )
        .is_empty());
        // …included when the hashes match…
        let owned = Snippet {
            sync_account: Some("hash-b".to_string()),
            ..snippet("8", "brb", "be right back")
        };
        assert_eq!(
            active_snippet_expansion_keys_from(&shield_store(true, vec![owned]), Some("hash-b"))
                .len(),
            1
        );
        // …and excluded when the row has an account but none is active.
        let orphan = Snippet {
            sync_account: Some("hash-b".to_string()),
            ..snippet("9", "gtg", "got to go")
        };
        assert!(
            active_snippet_expansion_keys_from(&shield_store(true, vec![orphan]), None).is_empty()
        );
    }

    #[test]
    fn test_active_snippet_keys_case_insensitive_canonical() {
        let keys = active_snippet_expansion_keys_from(
            &shield_store(true, vec![snippet("1", "AGAIN", "Agent")]),
            None,
        );
        assert!(keys.contains(&crate::dictionary::canonical_entry_key("again", "agent")));
    }

    // ── Round-4 F3: fail-open seam of the echo shield ────────────────

    #[test]
    fn test_shield_keys_fail_open_on_unreadable_store() {
        // A read error yields an EMPTY key set - the shield can never block
        // learning. The unreadable-store warn is recorded inside
        // cached_store_checked's load-error branch, once per cache
        // generation, regardless of call order.
        let keys = super::shield_keys_for(Err(anyhow::anyhow!("injected unreadable store")), None);
        assert!(keys.is_empty());
    }

    #[test]
    fn test_shield_keys_empty_with_cache_populated_degraded() {
        // Reviewer Round-4 revalidation (the ordering gap): in real
        // `process_transcript` order the expansion path's `cached_store()`
        // runs FIRST and, on an unreadable store, leaves the degraded
        // default cached; the echo shield wrapper is reached LATER and
        // must still yield an EMPTY key set off that cache hit - never a
        // panic, never treated as a fresh store problem. Seed the cache
        // exactly as a degraded load leaves it (directly, no fs - the F7
        // shared-root zone stays untouched).
        let _guard = crate::sync::stores::store_test_guard();
        let mut cache = STORE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        *cache = Some(SnippetStore::default());
        drop(cache);

        let keys = active_snippet_expansion_keys();
        assert!(
            keys.is_empty(),
            "degraded cached store must yield an empty shield key set: {keys:?}"
        );
        // Leave the cache unpopulated so later tests load fresh state.
        invalidate_cache();
    }

    #[test]
    fn test_shield_keys_ok_path_filters_normally() {
        let store = shield_store(true, vec![snippet("1", "again", "agent")]);
        let keys = super::shield_keys_for(Ok(store), None);
        assert_eq!(
            keys.iter().collect::<Vec<_>>(),
            vec![&crate::dictionary::canonical_entry_key("again", "agent")]
        );
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

    // --- corrupt-file recovery (Phase E) ----------------------------------
    //
    // A malformed snippets.json must surface an error with the original bytes
    // preserved aside — never silently become an empty store that a later
    // write persists over the evidence.

    fn corrupt_cleanup(path: &std::path::Path) {
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
    fn corrupt_snippets_file_errors_and_preserves_bytes() {
        // Hermetic: unique temp path, no shared test data root involved.
        let path = std::env::temp_dir().join(format!(
            "fluence-snip-test-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let garbage = b"[{\"trigger\": broken";
        std::fs::write(&path, garbage).unwrap();

        let loaded = load_store_file(&path);
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
    fn snippet_save_merged_refuses_corrupt_file_then_recovers_fresh() {
        use crate::sync::domain::SnippetItem;
        use crate::sync::error::SyncError;
        use crate::sync::frozen::DirtyStore;
        use crate::sync::stores::SnippetDirtyStore;
        let _guard = crate::sync::stores::store_test_guard();
        let path = snippets_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        corrupt_cleanup(&path);
        // Seed a valid store first so the corruption replaces real state.
        save_store_internal(&SnippetStore::default()).unwrap();
        let garbage = b"{{{{not json";
        std::fs::write(&path, garbage).unwrap();

        let mut store = SnippetDirtyStore;
        let winner = SnippetItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            trigger: "addr".to_string(),
            expansion: "address".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 100,
            device_id: "device-a".to_string(),
        };
        let account_hash = format!("{:064x}", 9u128);
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

        let mut store = SnippetDirtyStore;
        store
            .save_merged(&account_hash, vec![winner])
            .expect("fresh store after quarantine must accept writes");
        let after = load_store_internal().expect("fresh store must load");
        assert_eq!(after.snippets.len(), 1);
        assert_eq!(after.snippets[0].trigger, "addr");
        corrupt_cleanup(&path);
        if path.exists() {
            if let Ok(mut store) = load_store_internal() {
                store
                    .snippets
                    .retain(|s| s.sync_account.as_deref() != Some(account_hash.as_str()));
                let _ = save_store_internal(&store);
            }
        }
        corrupt_cleanup(&path);
    }
}
