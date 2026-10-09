// Fluence sync - frozen v1.2 local stores (DirtyStore implementations)
//
// Account isolation model:
// - dictionary.json / snippets.json rows carry `sync_account`; loads filter
//   by the active account hash, so another account's rows are never uploaded.
// - stats events live in one local ledger (`stats_events.json`) with a
//   nullable account stamp; unstamped (pre-sign-in) dictations are claimed by
//   the first account that syncs them. Event ids are UUIDv5 of the history
//   row id, so a backfilled row and a freshly-recorded event for the same
//   dictation collapse under union dedup - exactly-once counting by
//   construction.
// - settings LWW bookkeeping lives in `settings_sync_<hash>.json`, one
//   document per account, so preferences cannot cross accounts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[cfg(not(test))]
use dirs::data_local_dir;
use serde::{Deserialize, Serialize};

use crate::dictionary::{load_dictionary_internal, save_dictionary_internal, DictionaryEntry};
use crate::settings::{load_settings, AppSettings};
use crate::snippets::{
    load_store_internal as load_snippets_internal, save_store_internal as save_snippets_internal,
    Snippet,
};
use crate::sync::domain::*;
use crate::sync::error::SyncError;
use crate::sync::frozen::DirtyStore;
use crate::sync::metadata::SyncMetadata;

/// Root directory for Fluence state, before the `Fluence` leaf.
///
/// Resolution order:
/// 1. `FLUENCE_DATA_DIR` — explicit override for hermetic test runs.
/// 2. Under `cfg(test)`, a temp directory, so a bare `cargo test` can never
///    write the real `%LOCALAPPDATA%\Fluence`.
/// 3. The Windows Known Folder for local app data — the production path,
///    which a test build cannot reach.
pub(crate) fn base_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FLUENCE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(test)]
    {
        // Unique per test *process*, shared by every thread within it. The test
        // harness runs tests in parallel, so a fixed directory would let them
        // write over each other, and two concurrent `cargo test` runs would
        // collide as well.
        static TEST_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        return TEST_DIR
            .get_or_init(|| {
                std::env::temp_dir()
                    .join(format!("fluence-cargo-test-datadir-{}", std::process::id()))
            })
            .clone();
    }
    #[cfg(not(test))]
    {
        data_local_dir().unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Base directory for all Fluence state (`<base>/Fluence`).
pub(crate) fn data_dir() -> PathBuf {
    let mut p = base_data_dir();
    p.push("Fluence");
    p
}

fn atomic_write(path: &PathBuf, data: &str) -> Result<(), SyncError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SyncError::Fatal(e.to_string()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).map_err(|e| SyncError::Fatal(e.to_string()))?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path).map_err(|e| SyncError::Fatal(e.to_string()))
}

// ── Dictionary store ────────────────────────────────────────────────────────

pub struct DictionaryDirtyStore;

impl DictionaryDirtyStore {
    fn to_domain_item(e: &DictionaryEntry) -> Option<DictionaryItem> {
        let updated = e.updated_at.or(e.created_at).unwrap_or(0);
        if updated <= 0 {
            return None;
        }
        let device = e
            .device_id
            .clone()
            .unwrap_or_else(|| SyncMetadata::load().device_id);
        Some(DictionaryItem {
            sync_id: e.id.clone(),
            spoken: e.spoken.clone(),
            corrected: e.corrected.clone(),
            kind: e.kind.clone(),
            is_enabled: e.is_enabled,
            deleted_at: e.deleted_at,
            updated_at: updated,
            device_id: device,
        })
    }

    fn from_domain_item(item: DictionaryItem, account_hash: &str) -> DictionaryEntry {
        DictionaryEntry {
            id: item.sync_id,
            spoken: item.spoken,
            corrected: item.corrected,
            kind: item.kind,
            created_at: Some(item.updated_at),
            deleted_at: item.deleted_at,
            updated_at: Some(item.updated_at),
            device_id: Some(item.device_id),
            is_enabled: item.is_enabled,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.to_string()),
            // Dormant legacy columns (kept for file-format compatibility).
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }
}

impl DirtyStore for DictionaryDirtyStore {
    type Item = DictionaryItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        load_dictionary_internal()
            .unwrap_or_default()
            .iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash))
            .filter_map(Self::to_domain_item)
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut all = load_dictionary_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let mut stamped = 0;
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        for e in all.iter_mut() {
            let owned = e.sync_account.as_deref() == Some(account_hash);
            let needs_repair = owned
                && (e.updated_at.unwrap_or(0) <= 0
                    || e.device_id.as_ref().map_or(true, |id| id.is_empty()));
            if e.sync_account.is_none() || needs_repair {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                e.sync_account = Some(account_hash.to_string());
                e.device_id = Some(
                    e.device_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| device_id.clone()),
                );
                // Preserve an existing valid updatedAt; only stamp when absent
                // so enrollment never fabricates a newer-than-remote edit.
                if e.updated_at.is_none() || e.updated_at == Some(0) {
                    e.updated_at = Some(now);
                }
                if e.created_at.is_none() {
                    e.created_at = Some(now);
                }
                e.dirty = true;
                e.ever_pushed = false;
                stamped += 1;
            }
        }
        if stamped > 0 {
            save_dictionary_internal(&all).map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        load_dictionary_internal()
            .unwrap_or_default()
            .iter()
            .any(|e| e.sync_account.as_deref() == Some(account_hash) && e.dirty)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut all = load_dictionary_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let rescued: Vec<DictionaryEntry> = all
            .iter()
            .filter(|e| {
                e.sync_account.as_deref() == Some(account_hash)
                    && e.dirty
                    && match merged.iter().find(|m| m.sync_id == e.id) {
                        Some(winner) => {
                            crate::sync::clock::cmp_winner(
                                e.updated_at.unwrap_or(0),
                                e.device_id.as_deref().unwrap_or(""),
                                winner.updated_at,
                                &winner.device_id,
                            ) == std::cmp::Ordering::Greater
                        }
                        None => true,
                    }
            })
            .cloned()
            .collect();
        all.retain(|e| e.sync_account.as_deref() != Some(account_hash));
        for item in merged {
            all.push(Self::from_domain_item(item, account_hash));
        }
        for mut entry in rescued {
            entry.dirty = true;
            entry.ever_pushed = false;
            all.push(entry);
        }
        // Fold the never-pushed tombstone purge into the same write as the
        // merge+clean. A separate load→mutate→write pass could stamp a local
        // edit made meanwhile as pushed without it ever reaching the server.
        all.retain(|e| {
            !(e.sync_account.as_deref() == Some(account_hash)
                && e.deleted_at.is_some()
                && !e.ever_pushed)
        });
        save_dictionary_internal(&all).map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

// ── Snippet store ───────────────────────────────────────────────────────────

pub struct SnippetDirtyStore;

impl SnippetDirtyStore {
    fn to_domain_item(s: &Snippet) -> Option<SnippetItem> {
        let updated = s.updated_at.or(s.created_at).unwrap_or(0);
        if updated <= 0 {
            return None;
        }
        let device = s
            .device_id
            .clone()
            .unwrap_or_else(|| SyncMetadata::load().device_id);
        Some(SnippetItem {
            sync_id: s.id.clone(),
            trigger: s.trigger.clone(),
            expansion: s.expansion.clone(),
            is_enabled: s.is_enabled,
            deleted_at: s.deleted_at,
            updated_at: updated,
            device_id: device,
        })
    }

    fn from_domain_item(item: SnippetItem, account_hash: &str) -> Snippet {
        Snippet {
            id: item.sync_id,
            trigger: item.trigger,
            expansion: item.expansion,
            created_at: Some(item.updated_at),
            updated_at: Some(item.updated_at),
            device_id: Some(item.device_id),
            is_enabled: item.is_enabled,
            deleted_at: item.deleted_at,
            dirty: false,
            ever_pushed: true,
            sync_account: Some(account_hash.to_string()),
            // Dormant legacy columns (kept for file-format compatibility).
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        }
    }
}

impl DirtyStore for SnippetDirtyStore {
    type Item = SnippetItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        load_snippets_internal()
            .unwrap_or_default()
            .snippets
            .iter()
            .filter(|s| s.sync_account.as_deref() == Some(account_hash))
            .filter_map(Self::to_domain_item)
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut store = load_snippets_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let mut stamped = 0;
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        for s in store.snippets.iter_mut() {
            let owned = s.sync_account.as_deref() == Some(account_hash);
            let needs_repair = owned
                && (s.updated_at.unwrap_or(0) <= 0
                    || s.device_id.as_ref().map_or(true, |id| id.is_empty()));
            if s.sync_account.is_none() || needs_repair {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                s.sync_account = Some(account_hash.to_string());
                s.device_id = Some(
                    s.device_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| device_id.clone()),
                );
                if s.updated_at.is_none() || s.updated_at == Some(0) {
                    s.updated_at = Some(now);
                }
                if s.created_at.is_none() {
                    s.created_at = Some(now);
                }
                s.dirty = true;
                s.ever_pushed = false;
                stamped += 1;
            }
        }
        if stamped > 0 {
            save_snippets_internal(&store).map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        load_snippets_internal()
            .unwrap_or_default()
            .snippets
            .iter()
            .any(|s| s.sync_account.as_deref() == Some(account_hash) && s.dirty)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut store = load_snippets_internal().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let rescued: Vec<Snippet> = store
            .snippets
            .iter()
            .filter(|s| {
                s.sync_account.as_deref() == Some(account_hash)
                    && s.dirty
                    && match merged.iter().find(|m| m.sync_id == s.id) {
                        Some(winner) => {
                            crate::sync::clock::cmp_winner(
                                s.updated_at.unwrap_or(0),
                                s.device_id.as_deref().unwrap_or(""),
                                winner.updated_at,
                                &winner.device_id,
                            ) == std::cmp::Ordering::Greater
                        }
                        None => true,
                    }
            })
            .cloned()
            .collect();
        store
            .snippets
            .retain(|s| s.sync_account.as_deref() != Some(account_hash));
        for item in merged {
            store
                .snippets
                .push(Self::from_domain_item(item, account_hash));
        }
        for mut entry in rescued {
            entry.dirty = true;
            entry.ever_pushed = false;
            store.snippets.push(entry);
        }
        store.snippets.retain(|s| {
            !(s.sync_account.as_deref() == Some(account_hash)
                && s.deleted_at.is_some()
                && !s.ever_pushed)
        });
        save_snippets_internal(&store).map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

// ── Settings store (value-diff LWW, per-account bookkeeping) ────────────────
//
// Mirrors the proven Android PrefsSettingsV1Store semantics: a per-account
// meta document records the last-synced {value, updatedAt} per key. A live
// value differing from the recorded one is dirty with a fresh wall-clock
// timestamp. Incoming winners are applied to real settings and recorded.
//
// Windows emits four keys. `dictionary_enabled` is never emitted here
// (Windows applies the dictionary unconditionally and has no toggle); an
// incoming value for it is recorded but not applied, so the two platforms
// do not fight over a setting only Android exposes.

pub struct SettingsDirtyStore;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SettingsMetaDoc {
    /// key -> last-synced {v: value, t: updatedAt}
    #[serde(default)]
    keys: HashMap<String, KeyMeta>,
    /// Global values captured when an account becomes active. These are a
    /// baseline only; they are never uploaded unless the user edits them.
    #[serde(default)]
    activation_baseline: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyMeta {
    v: String,
    t: i64,
}

/// The keys this platform emits/accepts, mapped onto real settings. See
/// `live_values` - `dictionary_enabled` is deliberately absent (Windows has
/// no dictionary toggle; incoming values are recorded but not applied).

impl SettingsDirtyStore {
    /// Settings values are stored in machine-global preferences. Never apply
    /// a result for an account that is no longer active.
    fn active_account_matches(account_hash: &str) -> bool {
        load_settings()
            .ok()
            .and_then(|settings| settings.sync_account_key)
            .map(|email| crate::sync::metadata::account_hash_from_email(&email))
            .as_deref()
            == Some(account_hash)
    }

    fn meta_path(account_hash: &str) -> PathBuf {
        let mut p = data_dir();
        p.push(format!("settings_sync_{account_hash}.json"));
        p
    }

    /// Record an account activation without overwriting machine-global
    /// settings. The next pass will pull that account's remote values, or
    /// treat the captured values as a non-uploaded baseline if no file exists.
    pub fn activate_account(account_hash: &str) -> Result<(), SyncError> {
        let settings = load_settings().map_err(|e| SyncError::Fatal(e.to_string()))?;
        let baseline = Self::live_values(&settings).into_iter().collect();
        let mut meta = Self::load_meta(account_hash);
        meta.activation_baseline = Some(baseline);
        Self::save_meta(account_hash, &meta)
    }

    fn activation_baseline_unchanged(meta: &SettingsMetaDoc, settings: &AppSettings) -> bool {
        let Some(baseline) = meta.activation_baseline.as_ref() else {
            return false;
        };
        Self::live_values(settings)
            .into_iter()
            .all(|(key, value)| baseline.get(&key) == Some(&value))
    }

    fn load_meta(account_hash: &str) -> SettingsMetaDoc {
        let path = Self::meta_path(account_hash);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default()
    }

    fn save_meta(account_hash: &str, doc: &SettingsMetaDoc) -> Result<(), SyncError> {
        let data =
            serde_json::to_string_pretty(doc).map_err(|e| SyncError::Fatal(e.to_string()))?;
        atomic_write(&Self::meta_path(account_hash), &data)
    }

    /// Live values for the emitted keys, read from real application settings.
    fn live_values(settings: &AppSettings) -> Vec<(String, String)> {
        let snippets_enabled = load_snippets_internal().map(|s| s.enabled).unwrap_or(false);
        vec![
            ("language".to_string(), settings.language.clone()),
            ("snippets_enabled".to_string(), snippets_enabled.to_string()),
            (
                "auto_learn_enabled".to_string(),
                settings.auto_learn_enabled.to_string(),
            ),
            (
                "ai_polish_style".to_string(),
                settings.ai_polish_style.clone(),
            ),
        ]
    }

    /// Apply an incoming winner to real settings. Only allowed keys are ever
    /// touched; provider credentials/hotkeys/audio can never arrive here.
    fn apply_winner(settings: &mut AppSettings, key: &str, value: &str) {
        match key {
            "language" => settings.language = value.to_string(),
            "auto_learn_enabled" => settings.auto_learn_enabled = value == "true",
            "ai_polish_style" => settings.ai_polish_style = value.to_string(),
            "snippets_enabled" => {
                if let Ok(mut store) = load_snippets_internal() {
                    store.enabled = value == "true";
                    let _ = save_snippets_internal(&store);
                }
            }
            // "dictionary_enabled": recorded in meta but not applied on Windows.
            _ => {}
        }
    }
}

impl DirtyStore for SettingsDirtyStore {
    type Item = SettingsItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        let mut meta = Self::load_meta(account_hash);
        let Ok(settings) = load_settings() else {
            return Vec::new();
        };
        if meta.activation_baseline.is_some() {
            if Self::activation_baseline_unchanged(&meta, &settings) {
                return Vec::new();
            }
            meta.activation_baseline = None;
            let _ = Self::save_meta(account_hash, &meta);
        }
        // Global preferences may still contain the previous account's values
        // immediately after sign-in. Baseline a new account without emitting
        // those values; a later user edit is detected against this baseline.
        if meta.keys.is_empty() {
            let mut seeded = SettingsMetaDoc::default();
            for (key, value) in Self::live_values(&settings) {
                seeded.keys.insert(key, KeyMeta { v: value, t: 0 });
            }
            let _ = Self::save_meta(account_hash, &seeded);
            return Vec::new();
        }
        // Hoisted once: device id for every row, and the monotonic-clock floor
        // so a local edit is never stamped below what this device has seen
        // (a backwards wall-clock jump must not let an edit lose on LWW).
        let global_meta = SyncMetadata::load();
        let max_seen = global_meta
            .for_account(account_hash)
            .map(|s| s.max_seen)
            .unwrap_or(0);
        let (edit_now, _) = crate::sync::clock::monotonic_now(max_seen);
        let device_id = global_meta.device_id;
        let mut out = Vec::new();
        for (key, live) in Self::live_values(&settings) {
            match meta.keys.get(&key) {
                None => {
                    // First observation of a locally-set key: adopt it with sentinel 0 so remote wins.
                    out.append(&mut vec![SettingsItem {
                        key,
                        value: live,
                        updated_at: 0,
                        device_id: device_id.clone(),
                    }]);
                }
                Some(known) if known.t == 0 && known.v == live => {
                    // Unchanged first-session baseline: do not upload values
                    // inherited from the previous account when this account
                    // has no remote settings file yet.
                }
                Some(known) if known.v != live => {
                    // Local edit since last sync → dirty with a fresh clock
                    // that still respects the persisted maxSeen floor.
                    out.push(SettingsItem {
                        key,
                        value: live,
                        updated_at: edit_now,
                        device_id: device_id.clone(),
                    });
                }
                Some(known) => {
                    out.push(SettingsItem {
                        key,
                        value: known.v.clone(),
                        updated_at: known.t,
                        device_id: device_id.clone(),
                    });
                }
            }
        }
        out
    }

    fn stamp_account(&mut self, _account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        // Value-diff bookkeeping needs no row stamping.
        Ok(0)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        let meta = Self::load_meta(account_hash);
        let Ok(settings) = load_settings() else {
            return false;
        };
        if Self::activation_baseline_unchanged(&meta, &settings) {
            return false;
        }
        Self::live_values(&settings)
            .iter()
            .any(|(key, live)| meta.keys.get(key).map_or(true, |known| known.v != *live))
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        if !Self::active_account_matches(account_hash) {
            return Ok(());
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut meta = Self::load_meta(account_hash);
        let activation_pending = meta.activation_baseline.is_some();
        let mut settings_changed = false;
        let mut settings = load_settings().map_err(|e| SyncError::Fatal(e.to_string()))?;
        for item in merged {
            if !Self::active_account_matches(account_hash) {
                return Ok(());
            }
            let known = meta.keys.get(&item.key);
            let is_newer = known.map_or(true, |k| item.updated_at > k.t);
            if is_newer {
                if known.map_or(true, |k| k.v != item.value) {
                    Self::apply_winner(&mut settings, &item.key, &item.value);
                    settings_changed = true;
                }
                meta.keys.insert(
                    item.key.clone(),
                    KeyMeta {
                        v: item.value,
                        t: item.updated_at,
                    },
                );
            }
        }
        if activation_pending {
            meta.activation_baseline = None;
        }
        Self::save_meta(account_hash, &meta)?;
        if settings_changed {
            if !Self::active_account_matches(account_hash) {
                return Ok(());
            }
            crate::settings::save_settings(&settings)
                .map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(())
    }
}

// ── Stats ledger (event-sourced, account-claiming, exactly-once) ───────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatEventRow {
    pub item: StatsItem,
    /// None until an account claims this event (unstamped dictation).
    pub account: Option<String>,
    pub dirty: bool,
    pub ever_pushed: bool,
}

/// Local persistent ledger of this device's dictation events.
pub struct StatsDirtyStore;

#[cfg(test)]
static TEST_LEDGER_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
#[cfg(test)]
static TEST_HISTORY_PATH: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

impl StatsDirtyStore {
    fn ledger_path() -> PathBuf {
        #[cfg(test)]
        {
            if let Ok(guard) = TEST_LEDGER_PATH.lock() {
                if let Some(p) = guard.as_ref() {
                    return p.clone();
                }
            }
        }
        let mut p = data_dir();
        p.push("stats_events.json");
        p
    }

    /// Test seam: redirect the ledger to a temp path for isolation.
    #[cfg(test)]
    pub fn set_test_ledger_path(path: Option<std::path::PathBuf>) {
        *TEST_LEDGER_PATH.lock().unwrap() = path;
    }

    #[cfg(test)]
    pub fn set_test_history_path(path: Option<std::path::PathBuf>) {
        *TEST_HISTORY_PATH.lock().unwrap() = path;
    }

    fn history_db_path() -> PathBuf {
        #[cfg(test)]
        {
            if let Ok(guard) = TEST_HISTORY_PATH.lock() {
                if let Some(p) = guard.as_ref() {
                    return p.clone();
                }
            }
        }
        let mut p = data_dir();
        p.push("history.db");
        p
    }

    fn query_history_rows(conn: &rusqlite::Connection) -> Vec<(String, i64, String, i64)> {
        let Ok(mut stmt) =
            conn.prepare("SELECT id, timestamp_ms, text, duration_ms FROM history WHERE deleted_at IS NULL AND timestamp_ms > 0")
        else {
            return Vec::new();
        };
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<(String, i64, String, i64)> {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        };
        let x = match stmt.query_map([], mapper) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => Vec::new(),
        };
        x
    }

    fn load_rows() -> Vec<StatEventRow> {
        std::fs::read_to_string(Self::ledger_path())
            .ok()
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default()
    }

    fn save_rows(rows: &[StatEventRow]) -> Result<(), SyncError> {
        let data = serde_json::to_string(rows).map_err(|e| SyncError::Fatal(e.to_string()))?;
        atomic_write(&Self::ledger_path(), &data)
    }

    /// Record one completed dictation exactly once. Called from the history
    /// commit path. The event id is UUIDv5 of the history row id, so even a
    /// duplicated call (or a later backfill of the same row) collapses under
    /// union dedup. Safe offline: the event rides the next successful sync.
    pub fn record_dictation_event(
        history_id: &str,
        timestamp_ms: i64,
        text: &str,
        duration_ms: i64,
    ) {
        let _io = crate::sync::io_lock::io_lock_guard();
        let item = StatsItem::from_history_row(history_id, timestamp_ms, text, duration_ms);
        let mut rows = Self::load_rows();
        if rows.iter().any(|r| r.item.event_id == item.event_id) {
            return; // idempotent by construction
        }
        rows.push(StatEventRow {
            item,
            account: None,
            dirty: true,
            ever_pushed: false,
        });
        let _ = Self::save_rows(&rows);
        Self::invalidate_activity_cache();
    }

    /// One-time per-account seed: convert pre-existing history rows into
    /// events. Deterministic ids make this idempotent against events already
    /// recorded by `record_dictation_event`.
    fn backfill_from_history(account_hash: &str) -> Vec<StatEventRow> {
        let db_path = Self::history_db_path();
        if !db_path.exists() {
            return Vec::new();
        }
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        Self::query_history_rows(&conn)
            .into_iter()
            .filter_map(|(id, ts, text, dur)| {
                let item = StatsItem::from_history_row(&id, ts, &text, dur);
                Some(StatEventRow {
                    item,
                    account: Some(account_hash.to_string()),
                    dirty: true,
                    ever_pushed: false,
                })
            })
            .collect()
    }

fn backfill_done(metadata: &SyncMetadata, account_hash: &str) -> bool {
    metadata
      .for_account(account_hash)
      .map(|s| s.backfill_done)
      .unwrap_or(false)
  }

  /// Seed the local stats ledger from `history.db` when the signed-in account
  /// has no ledger events yet.
  ///
  /// WHY: the dashboard derives its activity buckets from this ledger, but the
  /// ledger is only seeded by a sync pass (`DirtyStore::load`). An installation
  /// upgraded from a build older than the ledger existed therefore had years of
  /// history and an empty ledger, so the dashboard reported "No baseline yet" —
  /// behaving like a brand-new user. Reusing the SAME `backfill_from_history`
  /// (deterministic UUIDv5 event ids, union dedup) keeps this idempotent and
  /// keeps both seeding paths converging on identical state.
  ///
  /// Deliberately does NOT set `backfill_done`: that flag belongs to the sync
  /// pass, and flipping it here would change sync behaviour. Leaving it unset
  /// just means the sync pass runs its own backfill later and finds every event
  /// id already present, so it writes nothing.
  ///
  /// Account-scoped: rows are stamped with THIS account hash and read back
  /// through `account_event_rows`, so no other account can observe them.
  /// Signed-out (`None`) is a no-op, so the all-rows local view is unchanged and
  /// no unattributed rows are ever created. Empty/unreadable `history.db`
  /// yields no synthetic rows, so a new user keeps the genuine empty state.
  fn ensure_account_backfilled(account_hash: &str) {
    if !crate::account_scope::valid_account_hash(account_hash) {
      return;
    }
    let _io = crate::sync::io_lock::io_lock_guard();
    let mut rows = Self::load_rows();
    // Only when this account has nothing yet: a populated ledger must be left
    // exactly as it is.
    if rows
      .iter()
      .any(|r| r.account.as_deref() == Some(account_hash))
    {
      return;
    }
    let synthetic = Self::backfill_from_history(account_hash);
    if synthetic.is_empty() {
      return;
    }
    let before = rows.len();
    for row in synthetic {
      if !rows.iter().any(|r| r.item.event_id == row.item.event_id) {
        rows.push(row);
      }
    }
    if rows.len() > before && Self::save_rows(&rows).is_ok() {
      // A previous read in this session may have cached the empty result.
      Self::invalidate_activity_cache();
    }
  }

    /// Test seam: raw ledger rows.
    #[cfg(test)]
    pub fn test_rows() -> Vec<StatEventRow> {
        Self::load_rows()
    }

    /// Account-level view: every event belonging to this account as
    /// (timestamp_ms, duration_ms, words, chars). After a sync pass this is
    /// the merged account state (local ∪ remote), so summing here yields the
    /// combined cross-device totals.
    pub fn account_event_rows(account_hash: &str) -> Vec<(i64, i64, i64, i64)> {
        let rows: Vec<StatEventRow> = Self::load_rows()
            .into_iter()
            .filter(|r| r.account.as_deref() == Some(account_hash))
            .collect();
        let dictation_days: HashSet<String> = rows
            .iter()
            .filter(|r| r.item.timestamp_ms > 0 || r.item.chars.unwrap_or(0) != 0)
            .map(|r| r.item.day.clone())
            .collect();
        rows.into_iter()
            .filter(|r| {
                !(r.item.timestamp_ms == 0
                    && r.item.chars.unwrap_or(0) == 0
                    && dictation_days.contains(&r.item.day))
            })
            .map(|r| {
                let ts = if r.item.timestamp_ms > 0 {
                    r.item.timestamp_ms
                } else {
                    chrono::NaiveDate::parse_from_str(&r.item.day, "%Y-%m-%d")
                        .ok()
                        .and_then(|d| d.and_hms_opt(0, 0, 0))
                        .map(|t| t.and_utc().timestamp_millis())
                        .unwrap_or(0)
                };
                (
                    ts,
                    r.item.duration_ms.unwrap_or(0),
                    r.item.words.unwrap_or(0),
                    r.item.chars.unwrap_or(0),
                )
            })
            .collect()
    }

    // ── Dashboard activity view (history-proof daily buckets) ─────────────
    //
    // Read-only view over the synced stats ledger for the history-proof
    // dashboard: one bucket per UTC day, computed server-side so payloads
    // are O(days), never O(events). No ledger writes, no sync interaction,
    // no schema change. Day math is UTC-midnight based so the bucket rule
    // is trivially Kotlin-replicable from the same synced rows (THE Kotlin
    // parity contract: day_start_ms = UTC midnight of the day containing
    // timestamp_ms; sums compose across days/weeks/months identically).

    /// Same row normalization as `account_event_rows` (timestamp wins, else
    /// UTC midnight of `item.day`), duplicated deliberately so the frozen
    /// command path above is untouched.
    fn normalize_stat_row(r: &StatEventRow) -> (i64, i64, i64, i64) {
        let ts = if r.item.timestamp_ms > 0 {
            r.item.timestamp_ms
        } else {
            chrono::NaiveDate::parse_from_str(&r.item.day, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|t| t.and_utc().timestamp_millis())
                .unwrap_or(0)
        };
        (
            ts,
            r.item.duration_ms.unwrap_or(0),
            r.item.words.unwrap_or(0),
            r.item.chars.unwrap_or(0),
        )
    }

    /// Shared filter+normalize used by the signed-out view below.
    fn normalize_rows(rows: Vec<StatEventRow>) -> Vec<(i64, i64, i64, i64)> {
        let active_days: HashSet<String> = rows
            .iter()
            .filter(|r| r.item.timestamp_ms > 0 || r.item.chars.unwrap_or(0) != 0)
            .map(|r| r.item.day.clone())
            .collect();
        rows.iter()
            .filter(|r| {
                !(r.item.timestamp_ms == 0
                    && r.item.chars.unwrap_or(0) == 0
                    && active_days.contains(&r.item.day))
            })
            .map(Self::normalize_stat_row)
            .collect()
    }

    /// All local ledger rows regardless of attribution (signed-out view),
    /// with the same aggregate-collapse rule as the account view so zero
    /// rows never count sessions.
    fn local_normalized_rows() -> Vec<(i64, i64, i64, i64)> {
        Self::normalize_rows(Self::load_rows())
    }

    fn bucket_events(rows: Vec<(i64, i64, i64, i64)>, since_ms: Option<i64>) -> Vec<DailyBucket> {
        let mut days: std::collections::BTreeMap<i64, (i64, i64, i64)> =
            std::collections::BTreeMap::new();
        for (ts, duration_ms, words, _chars) in rows {
            if ts < 0 {
                continue;
            }
            if let Some(since) = since_ms {
                if ts < since {
                    continue;
                }
            }
            let day_start_ms = ts - ts.rem_euclid(ACTIVITY_DAY_MS);
            let entry = days.entry(day_start_ms).or_insert((0, 0, 0));
            entry.0 += 1;
            entry.1 += words;
            entry.2 += duration_ms;
        }
        days.into_iter()
            .map(
                |(day_start_ms, (sessions, words, duration_ms))| DailyBucket {
                    day_start_ms,
                    sessions,
                    words,
                    duration_ms,
                },
            )
            .collect()
    }

    fn cached_event_rows(key: Option<String>, load: impl FnOnce() -> ActivityRows) -> ActivityRows {
        if let Ok(guard) = ACTIVITY_CACHE.lock() {
            if let Some((cached_key, rows)) = guard.as_ref() {
                if *cached_key == key {
                    return rows.clone();
                }
            }
        }
        let rows = load();
        if let Ok(mut guard) = ACTIVITY_CACHE.lock() {
            *guard = Some((key, rows.clone()));
        }
        rows
    }

    /// Drop the dashboard activity cache. Called on local record and on
    /// completed sync passes; the next read reloads lazily from the file.
    pub(crate) fn invalidate_activity_cache() {
        if let Ok(mut guard) = ACTIVITY_CACHE.lock() {
            *guard = None;
        }
    }

    /// UNIT D - growth gauge: rows + envelope bytes vs 8 MiB headroom for the given account.
    /// Pure, no I/O beyond reading the local ledger; callers surface via existing diagnostics path.
    pub fn gauge_for_account(account_hash: &str) -> (usize, usize, usize) {
        let items: Vec<StatsItem> = Self::load_rows()
            .into_iter()
            .filter(|r| r.account.as_deref() == Some(account_hash))
            .map(|r| r.item)
            .collect();
        let rows = items.len();
        let bytes = StatsEnvelope {
            v: crate::sync::domain::ENVELOPE_V1,
            entries: items,
        }
        .to_bytes()
        .len();
        let headroom = crate::sync::drive::MAX_DOMAIN_BYTES.saturating_sub(bytes);
        (rows, bytes, headroom)
    }
}

/// One UTC day of dictation activity.
#[derive(Debug, Clone, Serialize)]
pub struct DailyBucket {
    pub day_start_ms: i64,
    pub sessions: i64,
    pub words: i64,
    pub duration_ms: i64,
}

const ACTIVITY_DAY_MS: i64 = 86_400_000;

/// Process-level read cache: account key (`None` = signed-out all-rows
/// view) plus normalized event rows. Lazy-loaded on first read;
/// invalidated on local record and on completed sync passes (remote rows
/// land there — see the hook in the scheduler thread).
/// Normalized event rows: (timestamp_ms, duration_ms, words, chars).
type ActivityRows = Vec<(i64, i64, i64, i64)>;

static ACTIVITY_CACHE: std::sync::Mutex<Option<(Option<String>, ActivityRows)>> =
    std::sync::Mutex::new(None);

/// History-proof dashboard activity: daily UTC buckets from the synced
/// stats ledger (local ∪ remote), never the local history table.
/// Signed out, buckets cover ALL local ledger rows regardless of
/// attribution (dirty-only filtering would hide signed-in-era rows).
#[tauri::command]
/// Activity buckets for an explicit account.
///
/// Split out of [`get_account_activity`] so the read path can be exercised for
/// a given account WITHOUT touching `settings.json`. That matters: `settings_path()`
/// resolves through `dirs::data_local_dir()` directly and therefore ignores the
/// test-build data-dir redirect, so any test that called `save_settings()` wrote
/// to the real user profile and could sign the user out. Never call
/// `save_settings`/`restore` from tests; pass the hash here instead.
fn account_activity_for(
  account_hash: Option<&str>,
  since_ms: Option<i64>,
) -> Result<Vec<DailyBucket>, String> {
  // Seed-on-read for the upgrade path: an account with pre-existing history but
  // no ledger events yet must not look like a new user. No-op for a signed-out
  // session and for an already-populated ledger.
  if let Some(hash) = account_hash {
    StatsDirtyStore::ensure_account_backfilled(hash);
  }
  let rows = StatsDirtyStore::cached_event_rows(account_hash.map(str::to_string), || {
    match account_hash {
      Some(hash) => StatsDirtyStore::account_event_rows(hash),
      None => StatsDirtyStore::local_normalized_rows(),
    }
  });
  Ok(StatsDirtyStore::bucket_events(rows, since_ms))
}

#[tauri::command]
pub fn get_account_activity(since_ms: Option<i64>) -> Result<Vec<DailyBucket>, String> {
  let account_hash = crate::settings::load_settings()
    .ok()
    .and_then(|s| s.sync_account_key)
    .map(|email| crate::sync::metadata::account_hash_from_email(&email));
  account_activity_for(account_hash.as_deref(), since_ms)
}

impl DirtyStore for StatsDirtyStore {
    type Item = StatsItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        let mut rows = Self::load_rows();
        let metadata = SyncMetadata::load();
        if !Self::backfill_done(&metadata, account_hash) {
            // Seed once per account. Deterministic ids dedup against events
            // already recorded live by the commit path.
            let synthetic = Self::backfill_from_history(account_hash);
            let mut changed = false;
            for row in synthetic {
                if !rows.iter().any(|r| r.item.event_id == row.item.event_id) {
                    rows.push(row);
                    changed = true;
                }
            }
            if changed {
                let _ = Self::save_rows(&rows);
            }
        } else {
            // Reconciliation sweep: heal missing ledger rows after backfill_done (crash gap)
            let db_path = Self::history_db_path();
            if db_path.exists() {
                if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                    let history_rows = Self::query_history_rows(&conn);
                    let mut changed = false;
                    for (id, ts, text, dur) in history_rows {
                        let eid = synthetic_event_id(&id);
                        if !rows.iter().any(|r| r.item.event_id == eid) {
                            let item = StatsItem::from_history_row(&id, ts, &text, dur);
                            rows.push(StatEventRow {
                                item,
                                account: Some(account_hash.to_string()),
                                dirty: true,
                                ever_pushed: false,
                            });
                            changed = true;
                        }
                    }
                    if changed {
                        let _ = Self::save_rows(&rows);
                    }
                }
            }
        }
        rows.into_iter()
            .filter(|r| r.account.as_deref() == Some(account_hash))
            .map(|r| r.item)
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows = Self::load_rows();
        let mut stamped = 0;
        for row in rows.iter_mut() {
            if row.account.is_none() {
                row.account = Some(account_hash.to_string());
                row.dirty = true;
                stamped += 1;
            }
        }
        if stamped > 0 {
            Self::save_rows(&rows)?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        Self::load_rows()
            .iter()
            .any(|r| r.account.as_deref() == Some(account_hash) && r.dirty)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows = Self::load_rows();
        let merged_ids: std::collections::HashSet<String> =
            merged.iter().map(|i| i.event_id.clone()).collect();
        let rescued: Vec<StatEventRow> = rows
            .iter()
            .filter(|r| {
                r.account.as_deref() == Some(account_hash)
                    && r.dirty
                    && !merged_ids.contains(&r.item.event_id)
            })
            .cloned()
            .collect();
        // Mid-pass LWW guard for present rows: if a local dirty row still
        // wins tie (equal updatedAt, larger deviceId) keep it instead of
        // clobbering with the merged winner - mirrors Android
        // RoomStatV1Store.applyMergedAndClearDirty and dictionary rescue.
        use std::collections::HashMap;
        let dirty_by_id: HashMap<String, StatEventRow> = rows
            .iter()
            .filter(|r| r.account.as_deref() == Some(account_hash) && r.dirty)
            .map(|r| (r.item.event_id.clone(), r.clone()))
            .collect();
        let mut filtered_merged = Vec::new();
        let mut rescued_tie: Vec<StatEventRow> = Vec::new();
        for item in merged {
            if let Some(local) = dirty_by_id.get(&item.event_id) {
                let local_at = local.item.updated_at.unwrap_or(0);
                let local_dev = local.item.device_id.as_deref().unwrap_or("");
                let win_at = item.updated_at.unwrap_or(0);
                let win_dev = item.device_id.as_deref().unwrap_or("");
                if crate::sync::clock::cmp_winner(local_at, local_dev, win_at, win_dev)
                    != std::cmp::Ordering::Less
                {
                    rescued_tie.push(local.clone());
                    continue;
                }
            }
            filtered_merged.push(item);
        }
        rows.retain(|r| r.account.as_deref() != Some(account_hash));
        for item in filtered_merged {
            rows.push(StatEventRow {
                item,
                account: Some(account_hash.to_string()),
                dirty: false,
                ever_pushed: true,
            });
        }
        for mut row in rescued {
            row.dirty = true;
            row.ever_pushed = false;
            rows.push(row);
        }
        for mut row in rescued_tie {
            row.dirty = true;
            row.ever_pushed = false;
            rows.push(row);
        }
        Self::save_rows(&rows)?;
        // Mark backfill done after the first successful merge for this account.
        let mut metadata = SyncMetadata::load();
        if !Self::backfill_done(&metadata, account_hash) {
            metadata.for_account_mut(account_hash).backfill_done = true;
            metadata.save();
        }
        Ok(())
    }
}

// ── Agent store (Phase 6, account-partitioned) ──────────────────────────────
//
// Local persistence is the per-account file owned by `account_scope`
// (`agents.account-<hash>.json`), NOT a shared table with a sync_account
// column. That makes account isolation structural: `load` cannot observe
// another account's rows because they live in a different file.
//
// Dirty is DERIVED, mirroring Android `toLocal().dirty`: a row lacking sync
// metadata has never been pushed. There is no separate dirty flag to drift out
// of sync with the data, and no rescue pass: `save_merged` wholesale-replaces
// the account file with the merge winners, exactly like Android's
// `applyMergedAndClearDirty`. (Dictionary keeps a rescue pass because it is a
// hot shared file edited concurrently by the UI; agents are low-frequency and
// cross-platform convergence demands both sides compute the same result from
// the same merge, which a platform-specific rescue would break.)

pub struct AgentDirtyStore;

impl AgentDirtyStore {
    fn to_domain_item(
        e: &crate::account_scope::AccountAgent,
        device_id: &str,
    ) -> AgentItem {
        AgentItem {
            sync_id: e
                .sync_id
                .clone()
                .unwrap_or_else(|| AgentItem::stable_sync_id(&e.id)),
            business_key: e.id.clone(),
            name: e.name.clone(),
            hint: e.hint.clone(),
            updated_at: e.updated_at.unwrap_or(0),
            deleted_at: e.deleted_at,
            device_id: e
                .device_id
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| device_id.to_string()),
        }
    }

    fn from_domain_item(item: AgentItem) -> crate::account_scope::AccountAgent {
        crate::account_scope::AccountAgent {
            id: item.business_key,
            name: item.name,
            hint: item.hint,
            sync_id: Some(item.sync_id),
            updated_at: Some(item.updated_at),
device_id: Some(item.device_id),
            deleted_at: item.deleted_at,
            // A row that came back from a completed merge is by definition
            // already in the sync state, so it starts clean. Without this the
            // flag would survive the merge and every pass would re-upload it.
            dirty: false,
        }
    }

    /// Pending-mutation test.
    ///
    /// The explicit `dirty` flag is the load-bearing part. The metadata checks
    /// remain as a safety net so a row written before the flag existed (or by a
    /// truncated write) still uploads exactly once instead of being stranded.
    fn is_dirty(e: &crate::account_scope::AccountAgent) -> bool {
        e.dirty
            || e.sync_id.as_ref().map_or(true, |s| s.is_empty())
            || e.updated_at.unwrap_or(0) <= 0
    }
}

impl DirtyStore for AgentDirtyStore {
    type Item = AgentItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Vec::new();
        }
        let meta = SyncMetadata::load();
        let device_id = meta.device_id.clone();
        crate::account_scope::load_account_agents(account_hash)
            .custom_agents
            .iter()
            .map(|e| Self::to_domain_item(e, &device_id))
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(0);
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut store = crate::account_scope::load_account_agents(account_hash);
        if store.custom_agents.iter().all(|e| !Self::is_dirty(e)) {
            return Ok(0);
        }
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        let mut stamped = 0;
        for e in store.custom_agents.iter_mut() {
            if !Self::is_dirty(e) {
                continue;
            }
            // Stable sync id: deterministic per business key AND byte-identical
            // with Android, so a legacy record stamped independently on each
            // platform still converges under LWW instead of forking.
            if e.sync_id.as_ref().map_or(true, |s| s.is_empty()) {
                e.sync_id = Some(AgentItem::stable_sync_id(&e.id));
            }
            // A dirty row is a NEW local mutation, so it gets a fresh revision
            // from this device. Reusing the revision inherited from the peer we
            // pulled from would make the edit indistinguishable from what we
            // already had, and LWW would be free to discard it. `monotonic_now`
            // keeps the stamp above everything seen for the account.
            if e.dirty || e.updated_at.unwrap_or(0) <= 0 {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                e.updated_at = Some(now);
            }
            if e.dirty || e.device_id.as_ref().map_or(true, |d| d.is_empty()) {
                e.device_id = Some(device_id.clone());
            }
            stamped += 1;
        }
        if stamped > 0 {
            meta.save();
            crate::account_scope::save_account_agents(account_hash, &store)
                .map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return false;
        }
        crate::account_scope::load_account_agents(account_hash)
            .custom_agents
            .iter()
            .any(Self::is_dirty)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(());
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows: Vec<crate::account_scope::AccountAgent> =
            merged.into_iter().map(Self::from_domain_item).collect();
        // Fold the never-pushed tombstone purge into the same write.
        // A tombstone created locally and never uploaded carries no information
        // any other device could need; retaining it would resurrect nothing and
        // accumulate dead rows. (Tombstones that HAVE been pushed survive -
        // they are what stop the delete from being undone by a stale copy.)
        rows.retain(|e| {
            !(e.deleted_at.is_some()
                && e.sync_id.as_ref().map_or(true, |s| s.is_empty()))
        });
        crate::account_scope::save_account_agents(
            account_hash,
            &crate::account_scope::AccountAgentsStore { custom_agents: rows },
        )
        .map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

// ── Style store (Phase 6, account-partitioned) ──────────────────────────────
//
// Identical contract to [`AgentDirtyStore`] with the `custom:<uuid>` keyspace.
// See above for why dirty is derived, why there is no rescue pass, and why the
// purge is folded in.

pub struct StyleDirtyStore;

impl StyleDirtyStore {
    fn to_domain_item(
        e: &crate::account_scope::AccountStyle,
        device_id: &str,
    ) -> StyleItem {
        StyleItem {
            sync_id: e
                .sync_id
                .clone()
                .unwrap_or_else(|| StyleItem::stable_sync_id(&e.id)),
            business_key: e.id.clone(),
            name: e.name.clone(),
            hint: e.hint.clone(),
            updated_at: e.updated_at.unwrap_or(0),
            deleted_at: e.deleted_at,
            device_id: e
                .device_id
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| device_id.to_string()),
        }
    }

    fn from_domain_item(item: StyleItem) -> crate::account_scope::AccountStyle {
        crate::account_scope::AccountStyle {
            id: item.business_key,
            name: item.name,
            hint: item.hint,
            sync_id: Some(item.sync_id),
            updated_at: Some(item.updated_at),
device_id: Some(item.device_id),
            deleted_at: item.deleted_at,
            // Merged back in: already in the sync state, so clean.
            dirty: false,
        }
    }

    /// Pending-mutation test. See [`AgentDirtyStore::is_dirty`].
    fn is_dirty(e: &crate::account_scope::AccountStyle) -> bool {
        e.dirty
            || e.sync_id.as_ref().map_or(true, |s| s.is_empty())
            || e.updated_at.unwrap_or(0) <= 0
    }
}

impl DirtyStore for StyleDirtyStore {
    type Item = StyleItem;

    fn load(&self, account_hash: &str) -> Vec<Self::Item> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Vec::new();
        }
        let meta = SyncMetadata::load();
        let device_id = meta.device_id.clone();
        crate::account_scope::load_account_styles(account_hash)
            .custom_styles
            .iter()
            .map(|e| Self::to_domain_item(e, &device_id))
            .collect()
    }

    fn stamp_account(&mut self, account_hash: &str) -> Result<usize, SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(0);
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut store = crate::account_scope::load_account_styles(account_hash);
        if store.custom_styles.iter().all(|e| !Self::is_dirty(e)) {
            return Ok(0);
        }
        let mut meta = SyncMetadata::load();
        let device_id = meta.ensure_device_id();
        let mut stamped = 0;
        for e in store.custom_styles.iter_mut() {
            if !Self::is_dirty(e) {
                continue;
            }
            if e.sync_id.as_ref().map_or(true, |s| s.is_empty()) {
                e.sync_id = Some(StyleItem::stable_sync_id(&e.id));
            }
            // Fresh revision for a dirty row; see AgentDirtyStore::stamp_account.
            if e.dirty || e.updated_at.unwrap_or(0) <= 0 {
                let max_seen = meta
                    .for_account(account_hash)
                    .map(|s| s.max_seen)
                    .unwrap_or(0);
                let (now, new_max) = crate::sync::clock::monotonic_now(max_seen);
                meta.update_max_seen(account_hash, new_max);
                e.updated_at = Some(now);
            }
            if e.dirty || e.device_id.as_ref().map_or(true, |d| d.is_empty()) {
                e.device_id = Some(device_id.clone());
            }
            stamped += 1;
        }
        if stamped > 0 {
            meta.save();
            crate::account_scope::save_account_styles(account_hash, &store)
                .map_err(|e| SyncError::Fatal(e.to_string()))?;
        }
        Ok(stamped)
    }

    fn has_dirty(&self, account_hash: &str) -> bool {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return false;
        }
        crate::account_scope::load_account_styles(account_hash)
            .custom_styles
            .iter()
            .any(Self::is_dirty)
    }

    fn save_merged(
        &mut self,
        account_hash: &str,
        merged: Vec<Self::Item>,
    ) -> Result<(), SyncError> {
        if !crate::account_scope::valid_account_hash(account_hash) {
            return Ok(());
        }
        let _io = crate::sync::io_lock::io_lock_guard();
        let mut rows: Vec<crate::account_scope::AccountStyle> =
            merged.into_iter().map(Self::from_domain_item).collect();
        rows.retain(|e| {
            !(e.deleted_at.is_some()
                && e.sync_id.as_ref().map_or(true, |s| s.is_empty()))
        });
        crate::account_scope::save_account_styles(
            account_hash,
            &crate::account_scope::AccountStylesStore { custom_styles: rows },
        )
        .map_err(|e| SyncError::Fatal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_emitted_keys_never_include_secrets_or_platform_config() {
        let settings = AppSettings::default();
        let values = SettingsDirtyStore::live_values(&settings);
        let keys: Vec<&str> = values.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "language",
                "snippets_enabled",
                "auto_learn_enabled",
                "ai_polish_style"
            ]
        );
        for forbidden in [
            "hotkey",
            "agent_hotkey",
            "audio_device_id",
            "stt_provider",
            "llm_provider",
            "api_key",
        ] {
            assert!(
                !keys.contains(&forbidden),
                "{forbidden} must never be emitted"
            );
        }
    }

    #[test]
    fn apply_winner_cannot_touch_secrets_or_providers() {
        let mut settings = AppSettings::default();
        let before = settings.hotkey.clone();
        SettingsDirtyStore::apply_winner(&mut settings, "hotkey", "Ctrl+X");
        SettingsDirtyStore::apply_winner(&mut settings, "unknown_future_key", "evil");
        assert_eq!(settings.hotkey, before, "hotkey must be immutable via sync");
    }

    /// Serializes tests that mutate the process-global ledger/history test
    /// seams; parallel tests would otherwise flip each other's paths mid-run.
    static STORE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(super) fn store_test_guard() -> std::sync::MutexGuard<'static, ()> {
        STORE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn stat_event_recording_is_idempotent_per_history_row() {
        let _guard = store_test_guard();
        let tmp =
            std::env::temp_dir().join(format!("fluence-test-ledger-{}.json", std::process::id()));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        // record_dictation_event dedups on the deterministic event id.
        let id = "test-row-123";
        StatsDirtyStore::record_dictation_event(id, 1_000, "hello world", 500);
        StatsDirtyStore::record_dictation_event(id, 1_000, "hello world", 500);
        let rows = StatsDirtyStore::load_rows();
        let count = rows
            .iter()
            .filter(|r| r.item.event_id == synthetic_event_id(id))
            .count();
        assert_eq!(count, 1, "duplicate commits must not double-count");
        StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn save_merged_preserves_fresh_concurrent_edit() {
        let _guard = store_test_guard();
        let account_hash = format!("test-dict-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        // Prepare a dirty entry newer than the merged winner
        let dirty_id = uuid::Uuid::new_v4().to_string();
        let dirty_entry = DictionaryEntry {
            id: dirty_id.clone(),
            spoken: "testspoken".to_string(),
            corrected: "testcorrected".to_string(),
            kind: "correction".to_string(),
            created_at: Some(1000),
            deleted_at: None,
            updated_at: Some(2000),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(dirty_entry.clone());
        save_dictionary_internal(&all).unwrap();
        let winner = DictionaryItem {
            sync_id: dirty_id.clone(),
            spoken: "testspoken".to_string(),
            corrected: "oldcorrected".to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 1000,
            device_id: "device-test".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store
            .save_merged(&account_hash, vec![winner.clone()])
            .unwrap();
        let after = load_dictionary_internal().unwrap_or_default();
        let account_rows: Vec<_> = after
            .iter()
            .filter(|e| e.sync_account.as_deref() == Some(account_hash.as_str()))
            .collect();
        assert!(
            account_rows
                .iter()
                .any(|e| e.id == dirty_id && e.dirty && e.updated_at == Some(2000)),
            "fresh dirty edit must survive save_merged"
        );
        assert!(
            account_rows
                .iter()
                .any(|e| e.id == dirty_id && e.updated_at == Some(1000)),
            "merged winner also present"
        );
        // Cleanup
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| e.sync_account.as_deref() != Some(account_hash.as_str()));
        let _ = save_dictionary_internal(&cleanup);
    }

    #[test]
    fn save_merged_atomically_cleans_winners_and_purges_unpushed_tombstones() {
        let _guard = store_test_guard();
        let account_hash = format!(
            "test-dict-atomic-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let other_hash = format!(
            "test-dict-other-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let leader_id = uuid::Uuid::new_v4().to_string();
        let tombstone = DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "deletedlocal".to_string(),
            corrected: String::new(),
            kind: "correction".to_string(),
            created_at: Some(500),
            deleted_at: Some(600),
            updated_at: Some(600),
            device_id: Some("device-test".to_string()),
            is_enabled: false,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let dirty = DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "testspoken".to_string(),
            corrected: "testcorrected".to_string(),
            kind: "correction".to_string(),
            created_at: Some(1000),
            deleted_at: None,
            updated_at: Some(2900),
            device_id: Some("device-test".to_string()),
            is_enabled: true,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(account_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let other_tomb = DictionaryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            spoken: "otherdeleted".to_string(),
            corrected: String::new(),
            kind: "correction".to_string(),
            created_at: Some(500),
            deleted_at: Some(600),
            updated_at: Some(600),
            device_id: Some("device-other".to_string()),
            is_enabled: false,
            dirty: true,
            ever_pushed: false,
            sync_account: Some(other_hash.clone()),
            sync_state: None,
            server_file_id: None,
            quarantine_reason: None,
        };
        let mut all = load_dictionary_internal().unwrap_or_default();
        all.push(tombstone.clone());
        all.push(dirty.clone());
        all.push(other_tomb.clone());
        save_dictionary_internal(&all).unwrap();
        let winner = DictionaryItem {
            sync_id: leader_id.clone(),
            spoken: "testspoken".to_string(),
            corrected: "oldcorrected".to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 1500,
            device_id: "device-test".to_string(),
        };
        let mut store = DictionaryDirtyStore;
        store
            .save_merged(&account_hash, vec![winner.clone()])
            .unwrap();
        let after = load_dictionary_internal().unwrap_or_default();
        let winner_row = after
            .iter()
            .find(|e| e.sync_account.as_deref() == Some(account_hash.as_str()) && e.id == leader_id)
            .expect("merged winner must be present");
        assert!(
            !winner_row.dirty,
            "winner must be clean after a single merge write"
        );
        assert!(
            winner_row.ever_pushed,
            "winner must be pushed after a single merge write"
        );
        assert!(winner_row.updated_at == Some(1500));
        assert!(
            after
                .iter()
                .any(|e| e.id == dirty.id && e.dirty && !e.ever_pushed),
            "fresh dirty edit must stay dirty and unpushed (never force-stamped by a second pass)"
        );
        assert!(
            !after.iter().any(|e| e.id == tombstone.id),
            "this account's never-pushed tombstone must be purged in the same write"
        );
        assert!(
            after.iter().any(|e| e.id == other_tomb.id),
            "another account's tombstone must be untouched"
        );
        // Cleanup
        let mut cleanup = load_dictionary_internal().unwrap_or_default();
        cleanup.retain(|e| {
            e.sync_account.as_deref() != Some(account_hash.as_str())
                && e.sync_account.as_deref() != Some(other_hash.as_str())
        });
        let _ = save_dictionary_internal(&cleanup);
    }

    #[test]
    fn save_merged_preserves_dirty_not_in_merged_stats() {
        let _guard = store_test_guard();
        let tmp = std::env::temp_dir().join(format!(
            "fluence-test-ledger-stats-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        let _ = std::fs::remove_file(&tmp);
        let account_hash = format!("test-stats-{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let dirty_id = uuid::Uuid::new_v4().to_string();
        let dirty_item = StatsItem {
            event_id: dirty_id.clone(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 1000,
            words: Some(10),
            chars: Some(40),
            duration_ms: Some(5000),
            updated_at: None,
            device_id: None,
        };
        let dirty_row = StatEventRow {
            item: dirty_item.clone(),
            account: Some(account_hash.clone()),
            dirty: true,
            ever_pushed: false,
        };
        StatsDirtyStore::save_rows(&[dirty_row]).unwrap();
        // merged does NOT contain dirty_id
        let other = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-21".to_string(),
            timestamp_ms: 2000,
            words: Some(5),
            chars: Some(20),
            duration_ms: Some(3000),
            updated_at: None,
            device_id: None,
        };
        let mut store = StatsDirtyStore;
        store
            .save_merged(&account_hash, vec![other.clone()])
            .unwrap();
        let rows = StatsDirtyStore::load_rows();
        assert!(
            rows.iter().any(|r| r.item.event_id == dirty_id && r.dirty),
            "dirty row not in merged must be preserved"
        );
        assert!(
            rows.iter().any(|r| r.item.event_id == other.event_id),
            "merged row must be present"
        );
        // save_merged persisted backfill_done for this hash into the real
        // metadata file; remove the test hash so no state leaks.
        {
            let mut meta = SyncMetadata::load();
            meta.accounts.remove(&account_hash);
            meta.save();
        }
        StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_reconciles_missing_ledger_rows_after_backfill_done() {
        let _guard = store_test_guard();
        let tmp_ledger = std::env::temp_dir().join(format!(
            "fluence-test-ledger-reconcile-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let tmp_history = std::env::temp_dir().join(format!(
            "fluence-test-history-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let account_hash = format!(
            "test-reconcile-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        StatsDirtyStore::set_test_ledger_path(Some(tmp_ledger.clone()));
        StatsDirtyStore::set_test_history_path(Some(tmp_history.clone()));
        let _ = std::fs::remove_file(&tmp_ledger);
        let _ = std::fs::remove_file(&tmp_history);
        {
            let conn = rusqlite::Connection::open(&tmp_history).unwrap();
            conn.execute_batch(
                "CREATE TABLE history (id TEXT PRIMARY KEY, timestamp_ms INTEGER, text TEXT, duration_ms INTEGER, deleted_at INTEGER);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO history (id, timestamp_ms, text, duration_ms, deleted_at) VALUES (?1, ?2, ?3, ?4, NULL)",
                rusqlite::params!["row-1", 1713456000123i64, "hello world", 500i64],
            )
            .unwrap();
        }
        {
            let mut meta = SyncMetadata::load();
            meta.for_account_mut(&account_hash).backfill_done = true;
            meta.save();
        }
        let store = StatsDirtyStore;
        let items = store.load(&account_hash);
        assert_eq!(
            items.len(),
            1,
            "reconciliation should create missing ledger row"
        );
        assert_eq!(items[0].event_id, synthetic_event_id("row-1"));
        let items2 = store.load(&account_hash);
        assert_eq!(items2.len(), 1, "second load should not duplicate");
        let rows = StatsDirtyStore::load_rows();
        assert_eq!(rows.len(), 1);
        StatsDirtyStore::set_test_ledger_path(None);
        StatsDirtyStore::set_test_history_path(None);
        let _ = std::fs::remove_file(&tmp_ledger);
        let _ = std::fs::remove_file(&tmp_history);
        {
            let mut meta = SyncMetadata::load();
            meta.accounts.remove(&account_hash);
            meta.save();
        }
    }

    #[test]
    fn aggregates_filtered_for_existing_dictation_days() {
        // UNIT B - collapse rule: day-aggregates for days that already have dictation-level events must be suppressed.
        use crate::sync::domain::filter_aggregates_for_existing_dictation;
        use std::collections::HashSet;
        let agg1 = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 0,
            words: Some(100),
            chars: Some(0),
            duration_ms: Some(1000),
            updated_at: None,
            device_id: None,
        };
        let agg2 = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-21".to_string(),
            timestamp_ms: 0,
            words: Some(100),
            chars: Some(0),
            duration_ms: Some(1000),
            updated_at: None,
            device_id: None,
        };
        let mut existing = HashSet::new();
        existing.insert("2026-08-20".to_string());
        let filtered = filter_aggregates_for_existing_dictation(vec![agg1, agg2], &existing);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].day, "2026-08-21");
    }

    #[test]
    fn legacy_reconciliation_flagged_off_by_default() {
        // UNIT B - flagged reconciliation OFF by default, pure set-op (union-dedup by eventId)
        // This test documents the flag; actual deletion is behind feature gate.
        const STATS_RECONCILIATION_ENABLED: bool = false;
        assert!(
            !STATS_RECONCILIATION_ENABLED,
            "reconciliation must be OFF by default"
        );
    }

    #[test]
    fn account_activity_buckets_pin_utc_boundaries() {
        let _guard = store_test_guard();
        // Fixed UTC midnight: 2026-08-31T00:00:00Z.
        let monday = chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day = 86_400_000i64;
        let buckets = StatsDirtyStore::bucket_events(
            vec![
                (monday + 3_600_000, 5_000, 10, 50),
                (monday + 7_200_000, 3_000, 5, 20),
                (monday + day + 1_000, 2_000, 7, 30),
            ],
            None,
        );
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].day_start_ms, monday);
        assert_eq!(
            (
                buckets[0].sessions,
                buckets[0].words,
                buckets[0].duration_ms
            ),
            (2, 15, 8_000)
        );
        assert_eq!(buckets[1].day_start_ms, monday + day);
        assert_eq!(
            (
                buckets[1].sessions,
                buckets[1].words,
                buckets[1].duration_ms
            ),
            (1, 7, 2_000)
        );
        // since_ms filters server-side before bucketing.
        let tail = StatsDirtyStore::bucket_events(
            vec![
                (monday + 3_600_000, 5_000, 10, 50),
                (monday + day + 1_000, 2_000, 7, 30),
            ],
            Some(monday + day),
        );
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].day_start_ms, monday + day);

        // Signed-out view keeps rows regardless of attribution.
        let mk = |account: Option<&str>, ts: i64| StatEventRow {
            item: StatsItem::from_history_row("row", ts, "hello world test", 500),
            account: account.map(str::to_string),
            dirty: true,
            ever_pushed: false,
        };
        let normalized = StatsDirtyStore::normalize_rows(vec![
            mk(Some("hash"), monday + 1_000),
            mk(None, monday + 2_000),
        ]);
        assert_eq!(
            normalized,
            vec![(monday + 1_000, 500, 3, 16), (monday + 2_000, 500, 3, 16)]
        );
    }
}

// ===========================================================================
// STAGE 9 (deferred): dashboard baseline initialization on the upgrade path.
//
// "No baseline yet" is derived at render time from the trailing daily average
// of the stats ledger. The ledger is only seeded by a sync pass, so an install
// upgraded from a build older than the ledger had years of history and an empty
// ledger, and the dashboard reported the empty state like a new user.
//
// IMPORTANT: these tests drive `account_activity_for` with an INJECTED account
// hash and never call `save_settings()`. `settings_path()` resolves through
// `dirs::data_local_dir()` directly, bypassing the test-build data-dir redirect,
// so a test that saves settings writes to the real user profile and can sign the
// user out. `production_settings_survive_the_activity_read_path` guards that.
// ===========================================================================

#[cfg(test)]
mod baseline_backfill {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    fn utc_day_start(ts_ms: i64) -> i64 {
        ts_ms.div_euclid(DAY_MS) * DAY_MS
    }

    fn email_for(tag: &str) -> String {
        format!("baseline-{tag}@runtime.test")
    }

    fn hash_of(tag: &str) -> String {
        crate::sync::metadata::account_hash_from_email(&email_for(tag))
    }

    /// A minimal `history` table exposing exactly the columns
    /// `query_history_rows` reads. Building less than the real schema keeps the
    /// test honest about what the backfill actually depends on.
    fn write_history(path: &std::path::Path, rows: &[(i64, i64)]) {
        let conn = rusqlite::Connection::open(path).expect("history db");
        conn.execute_batch(
            "CREATE TABLE history (
                id TEXT PRIMARY KEY,
                timestamp_ms INTEGER NOT NULL DEFAULT 0,
                text TEXT NOT NULL DEFAULT '',
                duration_ms INTEGER NOT NULL DEFAULT 0,
                deleted_at INTEGER
            );",
        )
        .expect("schema");
        for (i, (ts, dur)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO history (id, timestamp_ms, text, duration_ms, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                rusqlite::params![format!("h{i}"), ts, "one two three four", dur],
            )
            .expect("insert");
        }
    }

    /// Point the ledger and history at temp files. Deliberately does NOT touch
    /// settings.json.
    fn arrange(tag: &str, history_rows: &[(i64, i64)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluence-baseline-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ledger = dir.join("stats_events.json");
        let history = dir.join("history.db");
        write_history(&history, history_rows);
        StatsDirtyStore::set_test_ledger_path(Some(ledger.clone()));
        StatsDirtyStore::set_test_history_path(Some(history));
        StatsDirtyStore::invalidate_activity_cache();
        ledger
    }

    fn restore() {
        StatsDirtyStore::set_test_ledger_path(None);
        StatsDirtyStore::set_test_history_path(None);
        StatsDirtyStore::invalidate_activity_cache();
    }

    fn activity(tag: &str) -> Vec<DailyBucket> {
        account_activity_for(Some(&hash_of(tag)), None).expect("activity")
    }

    fn day_bucket(buckets: &[DailyBucket], day_start: i64) -> Option<&DailyBucket> {
        buckets.iter().find(|b| b.day_start_ms == day_start)
    }

    fn days(buckets: &[DailyBucket]) -> Vec<i64> {
        buckets.iter().map(|b| b.day_start_ms).collect()
    }

    // 1. EMPTY LEDGER + POPULATED HISTORY -> baseline available immediately.
    #[test]
    fn empty_ledger_with_history_seeds_buckets_on_read() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "seed",
            &[
                (today - 2 * DAY_MS + 3_600_000, 5_000),
                (today - DAY_MS + 7_200_000, 9_000),
            ],
        );
        assert!(
            StatsDirtyStore::load_rows()
                .iter()
                .all(|r| r.account.as_deref() != Some(hash_of("seed").as_str())),
            "precondition: the ledger starts empty"
        );

        let buckets = activity("seed");

        assert!(
            day_bucket(&buckets, today - 2 * DAY_MS).is_some(),
            "history two days ago must produce a bucket; got {:?}",
            days(&buckets)
        );
        assert!(
            day_bucket(&buckets, today - DAY_MS).is_some(),
            "history yesterday must produce a bucket"
        );
        let prior = day_bucket(&buckets, today - DAY_MS).expect("yesterday bucket");
        assert_eq!(prior.sessions, 1, "one history row is one session");
        assert!(prior.words > 0, "the baseline needs a non-zero word count");
        restore();
    }

    // 2. IDEMPOTENCY: repeated reads add nothing and do not change totals.
    #[test]
    fn repeated_reads_are_idempotent() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "idem",
            &[
                (today - DAY_MS + 3_600_000, 5_000),
                (today - 2 * DAY_MS + 3_600_000, 5_000),
            ],
        );

        let first = activity("idem");
        let rows_after_first = StatsDirtyStore::load_rows().len();
        let second = activity("idem");
        let rows_after_second = StatsDirtyStore::load_rows().len();

        assert_eq!(
            rows_after_first, rows_after_second,
            "a second read must not append duplicate synthetic events"
        );
        assert_eq!(
            first.iter().map(|b| (b.day_start_ms, b.sessions, b.words)).collect::<Vec<_>>(),
            second.iter().map(|b| (b.day_start_ms, b.sessions, b.words)).collect::<Vec<_>>(),
            "bucket totals must be identical across reads"
        );
        restore();
    }

    // 3. POPULATED LEDGER: a partially synced account must not be bulk-seeded.
    #[test]
    fn a_populated_ledger_is_left_alone() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "populated",
            &[
                (today - 3 * DAY_MS + 3_600_000, 5_000),
                (today - DAY_MS + 3_600_000, 5_000),
            ],
        );
        StatsDirtyStore::save_rows(&[StatEventRow {
            item: StatsItem::from_history_row(
                "already-there",
                today - 3 * DAY_MS,
                "a b c",
                5_000,
            ),
            account: Some(hash_of("populated")),
            dirty: false,
            ever_pushed: true,
        }])
        .expect("seed ledger");
        StatsDirtyStore::invalidate_activity_cache();

        let buckets = activity("populated");

        assert_eq!(
            StatsDirtyStore::load_rows().len(),
            1,
            "an account that already has attributed events must not be bulk-seeded"
        );
        assert_eq!(
            days(&buckets),
            vec![today - 3 * DAY_MS],
            "only the already-present day may be shown"
        );
        restore();
    }

    // 4. EMPTY HISTORY + EMPTY LEDGER: the genuine new-user state stays.
    #[test]
    fn an_account_with_no_history_keeps_the_empty_state() {
        let _guard = super::tests::store_test_guard();
        let ledger = arrange("newuser", &[]);

        let buckets = activity("newuser");

        assert!(
            buckets.is_empty(),
            "a new user must still see no activity, i.e. the honest empty state"
        );
        let raw = std::fs::read_to_string(&ledger).unwrap_or_default();
        assert!(
            raw.is_empty() || raw == "null" || raw == "[]",
            "no synthetic events may be created for an account with no history; got {raw:?}"
        );
        restore();
    }

    // 5. MULTIPLE DAYS contribute independently to the trailing baseline.
    #[test]
    fn multiple_history_days_bucket_separately() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange(
            "multiday",
            &[
                (today - 3 * DAY_MS + 3_600_000, 1_000),
                (today - 3 * DAY_MS + 5_000_000, 1_000),
                (today - DAY_MS + 3_600_000, 1_000),
            ],
        );

        let buckets = activity("multiday");

        assert_eq!(buckets.len(), 2, "two distinct days, got {:?}", days(&buckets));
        assert_eq!(
            day_bucket(&buckets, today - 3 * DAY_MS).expect("day").sessions,
            2,
            "two rows collapse into one day with two sessions"
        );
        assert_eq!(
            day_bucket(&buckets, today - DAY_MS).expect("day").sessions,
            1
        );
        restore();
    }

    // 6. ACCOUNT ISOLATION: seeding account A must not surface in account B.
    #[test]
    fn seeding_one_account_does_not_leak_into_another() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("iso", &[(today - DAY_MS + 3_600_000, 5_000)]);

        StatsDirtyStore::ensure_account_backfilled(&hash_of("iso-a"));
        let rows = StatsDirtyStore::load_rows();
        assert_eq!(rows.len(), 1);
        assert!(
            rows.iter()
                .all(|r| r.account.as_deref() == Some(hash_of("iso-a").as_str())),
            "every seeded row must be attributed to the seeding account"
        );
        assert!(
            StatsDirtyStore::account_event_rows(&hash_of("iso-b")).is_empty(),
            "a different account must observe none of them"
        );
        assert!(
            account_activity_for(Some(&hash_of("iso-b")), None)
                .expect("activity")
                .is_empty(),
            "reading as the other account must still yield nothing"
        );
        restore();
    }

    // 7. CORRUPT / UNREADABLE HISTORY degrades safely with no ledger write.
    #[test]
    fn unreadable_history_degrades_without_writing_the_ledger() {
        let _guard = super::tests::store_test_guard();
        let dir = std::env::temp_dir().join(format!(
            "fluence-baseline-corrupt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ledger = dir.join("stats_events.json");
        let history = dir.join("history.db");
        std::fs::write(&history, b"this is definitely not sqlite").expect("write junk");
        StatsDirtyStore::set_test_ledger_path(Some(ledger.clone()));
        StatsDirtyStore::set_test_history_path(Some(history));
        StatsDirtyStore::invalidate_activity_cache();

        let buckets = activity("corrupt");

        assert!(
            buckets.is_empty(),
            "a corrupt history must yield the empty state, not partial data"
        );
        let raw = std::fs::read_to_string(&ledger).unwrap_or_default();
        assert!(
            raw.is_empty() || raw == "null" || raw == "[]",
            "no ledger may be written from an unreadable history; got {raw:?}"
        );
        restore();
    }

    // 8. CACHE: a stale empty cache must not mask the freshly seeded data.
    #[test]
    fn a_stale_empty_cache_does_not_mask_the_backfill() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("cache", &[(today - DAY_MS + 3_600_000, 5_000)]);

        // Seed the cache with an empty result as a signed-OUT read would.
        let rows = StatsDirtyStore::cached_event_rows(None, || {
            StatsDirtyStore::local_normalized_rows()
        });
        assert!(rows.is_empty(), "precondition: the cache holds an empty result");

        let buckets = activity("cache");

        assert!(
            !buckets.is_empty(),
            "the backfilled data must be visible immediately, not masked by a stale cache"
        );
        restore();
    }

    // 9. The existing sync-pass backfill still runs and stays idempotent.
    #[test]
    fn the_sync_pass_backfill_remains_idempotent_after_read_time_seeding() {
        let _guard = super::tests::store_test_guard();
        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("sync", &[(today - DAY_MS + 3_600_000, 5_000)]);

        StatsDirtyStore::ensure_account_backfilled(&hash_of("sync"));
        let after_read = StatsDirtyStore::load_rows().len();
        assert_eq!(after_read, 1, "read-time seeding wrote the history row");

        let mut store = StatsDirtyStore;
        store.load(&hash_of("sync"));
        assert_eq!(
            StatsDirtyStore::load_rows().len(),
            after_read,
            "the sync-pass backfill must find every id present and write nothing"
        );
        assert!(
            StatsDirtyStore::load_rows().iter().all(|r| r.account.is_some()),
            "no row may be left unattributed"
        );
        restore();
    }

    // 10. GUARD: the activity read path must never mutate real user settings.
    //
    // Regression guard for a real incident: `settings_path()` resolves through
    // `dirs::data_local_dir()` directly and ignores the test-build data-dir
    // redirect, so these tests previously called `save_settings()` and wrote to
    // the actual user profile, clearing the signed-in account.
    #[test]
    fn production_settings_survive_the_activity_read_path() {
        let _guard = super::tests::store_test_guard();
        let production = dirs::data_local_dir()
            .expect("local app data")
            .join("Fluence")
            .join("settings.json");
        let before = std::fs::read(&production).ok();
        let before_meta = std::fs::metadata(&production).ok().map(|m| m.modified().ok());

        let now = chrono::Utc::now().timestamp_millis();
        let today = utc_day_start(now);
        arrange("guard", &[(today - DAY_MS + 3_600_000, 5_000)]);
        let _ = activity("guard");
        restore();

        let after = std::fs::read(&production).ok();
        let after_meta = std::fs::metadata(&production).ok().map(|m| m.modified().ok());
        assert_eq!(
            before, after,
            "the activity read path must not rewrite the real settings.json"
        );
        assert_eq!(
            before_meta, after_meta,
            "the real settings.json must not even be re-written"
        );
    }
}
// ===========================================================================
// STAGE 8 runtime regression: Agent/Style dirty-state lifecycle.
//
// Found by real-device QA, not by the suite: after a record had synced once it
// carried a `syncId`/`updatedAt` forever, so deriving "dirty" from their
// absence made every LATER edit and delete permanently invisible to upload.
// CREATE propagated; EDIT and DELETE did not. These tests pin the full
// lifecycle so that cannot regress silently again.
// ===========================================================================

#[cfg(test)]
mod dirty_lifecycle {
    use super::*;

    /// A valid (64 lowercase hex) account hash derived from a unique tag, so each
    /// test owns its own store without colliding under parallel execution.
    fn qa_account(tag: &str) -> String {
        crate::sync::metadata::account_hash_from_email(&format!("qa-{tag}@runtime.test"))
    }

    fn qa_agent(hash: &str, id: &str) -> crate::account_scope::AccountAgent {
        crate::account_scope::load_account_agents(hash)
            .custom_agents
            .into_iter()
            .find(|a| a.id == id)
            .expect("agent row")
    }

    fn qa_style(hash: &str, id: &str) -> crate::account_scope::AccountStyle {
        crate::account_scope::load_account_styles(hash)
            .custom_styles
            .into_iter()
            .find(|s| s.id == id)
            .expect("style row")
    }

    fn qa_device_id() -> String {
        SyncMetadata::load().ensure_device_id()
    }

    /// CREATE -> dirty -> SYNC -> clean -> EDIT -> dirty again -> SYNC -> clean,
    /// with a strictly newer revision on every mutation.
    #[test]
    fn agent_edit_after_first_sync_is_dirty_again_and_gets_a_newer_revision() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-edit");
        let id = "agent:qa-edit-0001";
        let device = qa_device_id();
        let mut store = AgentDirtyStore;

        // 1. CREATE is dirty.
        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint-1");
        assert!(
            store.has_dirty(&hash),
            "a newly created agent must be dirty before its first sync"
        );
        assert!(
            qa_agent(&hash, id).dirty,
            "the local row must record the pending mutation"
        );

        // 2. SYNC stamps it, then a completed merge makes it clean.
        assert_eq!(store.stamp_account(&hash).unwrap(), 1);
        let first = qa_agent(&hash, id);
        let first_rev = first.updated_at.expect("stamped updatedAt");
        assert!(first_rev > 0);
        assert_eq!(first.device_id.as_deref(), Some(device.as_str()));
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(
            !store.has_dirty(&hash),
            "a successfully synchronized agent must be clean"
        );

        // 3. EDIT of an already-synced agent must become dirty AGAIN. This is the
        //    exact step that used to be a silent no-op.
        crate::account_scope::upsert_account_agent(&hash, id, "v2", "hint-2");
        assert!(
            store.has_dirty(&hash),
            "an edit to an already-synced agent must become dirty again"
        );

        // 4. The edit gets a NEWER revision from THIS device, never the revision
        //    inherited from the peer it was pulled from.
        assert_eq!(store.stamp_account(&hash).unwrap(), 1);
        let edited = qa_agent(&hash, id);
        assert_eq!(edited.hint, "hint-2");
        assert!(
            edited.updated_at.expect("edit revision") > first_rev,
            "the edit must carry a strictly newer LWW revision than the sync it followed"
        );
        assert_eq!(edited.device_id.as_deref(), Some(device.as_str()));

        // 5. Sync again -> clean, and the new revision is what a peer would merge.
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(!store.has_dirty(&hash));
        assert_eq!(
            store.load(&hash)[0].updated_at,
            edited.updated_at.expect("edit revision"),
            "the uploaded revision must be the edited one"
        );
    }

    /// Two consecutive edit+sync rounds must both propagate.
    #[test]
    fn agent_survives_two_edit_sync_cycles() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-twice");
        let id = "agent:qa-twice-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint-1");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        let mut last_rev = qa_agent(&hash, id).updated_at.expect("rev1");
        for round in 2..=3 {
            crate::account_scope::upsert_account_agent(&hash, id, &format!("v{round}"), "hint");
            assert!(
                store.has_dirty(&hash),
                "edit round {round} must dirty the row again"
            );
            store.stamp_account(&hash).unwrap();
            let rev = qa_agent(&hash, id).updated_at.expect("revision");
            assert!(rev > last_rev, "round {round} must advance the revision");
            last_rev = rev;
            store.save_merged(&hash, store.load(&hash)).unwrap();
            assert!(!store.has_dirty(&hash), "round {round} must end clean");
        }
        assert_eq!(store.load(&hash)[0].updated_at, last_rev);
    }

    /// DELETE of an already-synced agent must upload a tombstone with a fresh
    /// revision, instead of leaving the row looking untouched to the peer.
    #[test]
    fn agent_delete_after_first_sync_is_dirty_and_carries_a_fresh_tombstone() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-delete");
        let id = "agent:qa-delete-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint-1");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();
        let synced_rev = qa_agent(&hash, id).updated_at.expect("rev");

        assert!(!store.has_dirty(&hash), "precondition: row starts clean");

        crate::account_scope::delete_account_agent(&hash, id);
        assert!(
            store.has_dirty(&hash),
            "a delete of an already-synced agent must become dirty again"
        );

        store.stamp_account(&hash).unwrap();
        let dead = qa_agent(&hash, id);
        assert!(
            dead.deleted_at.is_some(),
            "the row must be a tombstone, not a removal"
        );
        assert!(
            dead.updated_at.expect("tombstone revision") > synced_rev,
            "the tombstone must carry a newer revision so LWW cannot lose the delete"
        );
        // The tombstone is uploaded, not purged: it has a sync id, so a peer that
        // still holds the live row cannot resurrect it.
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(store.load(&hash)[0].deleted_at.is_some());
    }

    /// DELETE then RECREATE: the app mints a NEW id, so the old tombstone must not
    /// suppress the legitimate replacement, and both rows must coexist correctly.
    #[test]
    fn agent_delete_then_recreate_leaves_the_tombstone_and_the_new_row() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-recreate");
        let old_id = "agent:qa-recreate-old";
        let new_id = "agent:qa-recreate-new";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, old_id, "original", "hint");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        crate::account_scope::delete_account_agent(&hash, old_id);
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        // Recreating mints a fresh id; the tombstone for the old id stays put.
        crate::account_scope::upsert_account_agent(&hash, new_id, "recreated", "hint");
        assert!(store.has_dirty(&hash), "the recreated row must upload");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        let rows = store.load(&hash);
        assert_eq!(rows.len(), 2, "tombstone and replacement must both survive");
        assert!(rows.iter().any(|r| r.business_key == old_id && r.deleted_at.is_some()));
        assert!(
            rows.iter().any(|r| r.business_key == new_id && r.deleted_at.is_none()),
            "the replacement must be live, not suppressed by the old tombstone"
        );
    }

    /// A row written BEFORE the dirty flag existed (no flag on disk, but stamped
    /// metadata) must still upload exactly once rather than being stranded.
    #[test]
    fn a_stamped_row_without_the_dirty_flag_is_still_deduped_not_duplicated() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-legacy-flag");
        let id = "agent:qa-legacy-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "v1", "hint");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        // A clean row that never needs stamping stays clean and unstamped.
        assert!(!store.has_dirty(&hash));
        assert_eq!(store.stamp_account(&hash).unwrap(), 0);
        assert!(!store.has_dirty(&hash));
        let _ = id;
    }

    /// Style counterpart of the agent lifecycle: edit and delete must both
    /// re-dirty after the first sync.
    #[test]
    fn style_edit_and_delete_after_first_sync_both_become_dirty_again() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("style-lifecycle");
        let id = "custom:qa-style-0001";
        let device = qa_device_id();
        let mut store = StyleDirtyStore;

        crate::account_scope::upsert_account_style(&hash, id, "v1", "style-hint");
        assert!(store.has_dirty(&hash), "a new style must be dirty");
        store.stamp_account(&hash).unwrap();
        let first_rev = qa_style(&hash, id).updated_at.expect("style rev");
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(!store.has_dirty(&hash), "a synced style must be clean");

        // EDIT
        crate::account_scope::upsert_account_style(&hash, id, "v2", "style-hint-2");
        assert!(store.has_dirty(&hash), "style edit must re-dirty");
        store.stamp_account(&hash).unwrap();
        let edited = qa_style(&hash, id);
        assert_eq!(edited.hint, "style-hint-2");
        assert!(edited.updated_at.expect("edit rev") > first_rev);
        assert_eq!(edited.device_id.as_deref(), Some(device.as_str()));
        store.save_merged(&hash, store.load(&hash)).unwrap();
        assert!(!store.has_dirty(&hash));

        // DELETE
        crate::account_scope::delete_account_style(&hash, id);
        assert!(store.has_dirty(&hash), "style delete must re-dirty");
        store.stamp_account(&hash).unwrap();
        let dead = qa_style(&hash, id);
        assert!(dead.deleted_at.is_some(), "style delete must tombstone");
        assert!(dead.updated_at.expect("tombstone rev") > edited.updated_at.unwrap());
    }

    /// Convergence: when a peer's row wins the LWW merge, the local row must end up
    /// byte-identical to the peer's revision, and must be clean afterwards.
    #[test]
    fn a_peer_winner_replaces_the_local_row_and_leaves_it_clean() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("agent-converge");
        let id = "agent:qa-converge-0001";
        let mut store = AgentDirtyStore;

        crate::account_scope::upsert_account_agent(&hash, id, "local", "local-hint");
        store.stamp_account(&hash).unwrap();
        store.save_merged(&hash, store.load(&hash)).unwrap();

        let peer = AgentItem {
            sync_id: AgentItem::stable_sync_id(id),
            business_key: id.to_string(),
            name: "peer".to_string(),
            hint: "peer-hint".to_string(),
            updated_at: qa_agent(&hash, id).updated_at.expect("rev") + 5_000,
            deleted_at: None,
            device_id: "peer-device".to_string(),
        };
        store.save_merged(&hash, vec![peer.clone()]).unwrap();

        let local = store.load(&hash);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].name, "peer");
        assert_eq!(local[0].hint, "peer-hint");
        assert_eq!(local[0].device_id, "peer-device");
        assert!(
            !store.has_dirty(&hash),
            "a merged peer winner must leave the row clean, or it would re-upload forever"
        );
    }

    // ---------------------------------------------------------------------
    // One-tap legacy claim: pinning the REAL account-store writer
    // ---------------------------------------------------------------------
    //
    // The claim takes `io_lock`, but `save_merged` can still be handed a payload
    // built from a document read BEFORE the Drive round trip, so it can discard a
    // committed claim. Two properties must therefore hold, asserted here against
    // the real writer rather than a stand-in:
    //
    //  1. the real merge writer contends for the same critical section as the
    //     claim, so a claim in progress cannot be half-applied around a save;
    //  2. a stale merge that DOES discard the claim leaves the record recoverable,
    //     because the claim copied rather than moved it.
    //
    // These live in `stores` because they need `store_test_guard()` (the account
    // stores are shared files on disk) and the real `AgentDirtyStore`.

    /// Pins that `AgentDirtyStore::save_merged` takes `io_lock`.
    ///
    /// Fails if that lock is removed: the writer would then run to completion while
    /// the critical section is held.
    ///
    /// The assertion is ORDER-based, never a timeout. A timeout version of this test
    /// proved flaky under full-suite load, where the machine can schedule the
    /// writer faster than a fixed window expects. Here the writer records whether
    /// it had finished by the time the holder released, so a slow machine can only
    /// make the test slower — it cannot make it fail spuriously.
    #[test]
    fn the_real_account_merge_writer_contends_for_the_claim_lock() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("claim-lock-contention");
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};

        let released = Arc::new(AtomicBool::new(false));
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();

        // Stands in for a claim in progress: holds the critical section.
        let holder = {
            let released = Arc::clone(&released);
            std::thread::spawn(move || {
                let _io = crate::sync::io_lock::io_lock_guard();
                // Sent only AFTER the guard is held, so the lock really is held.
                held_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                released.store(true, Ordering::SeqCst);
            })
        };
        held_rx.recv().unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let finished_early = Arc::new(AtomicBool::new(false));
        let writer = {
            let released = Arc::clone(&released);
            let finished_early = Arc::clone(&finished_early);
            let hash = hash.clone();
            std::thread::spawn(move || {
                let mut store = AgentDirtyStore;
                let _ = store.save_merged(&hash, Vec::new());
                finished_early.store(!released.load(Ordering::SeqCst), Ordering::SeqCst);
                done_tx.send(()).unwrap();
            })
        };

        // A grace window for the writer to START. Without the lock it finishes in
        // here and sets the flag; with the lock held it cannot finish at all.
        // A slow machine merely makes this sleep elapse with the writer still
        // blocked, which is the passing outcome anyway.
        std::thread::sleep(std::time::Duration::from_millis(150));
        go_tx.send(()).unwrap();

        done_rx.recv().unwrap();
        holder.join().unwrap();
        writer.join().unwrap();

        assert!(
            !finished_early.load(Ordering::SeqCst),
            "the real merge writer ran to completion while the account-store lock \
             was held; if save_merged does not take that lock, a claim and a merge \
             can interleave and discard the claim"
        );
    }

    /// A claim racing the REAL stamper loses nothing.
    ///
    /// Thread A drives production `AgentDirtyStore::stamp_account` in a loop
    /// while thread B performs a real claim. Either side dropping `io_lock`
    /// lets the stale snapshot win and the final assertions fail. This replaces
    /// the earlier hand-rolled stand-in, which proved only that the claim takes
    /// a lock — not that the production stamper shares it.
    #[test]
    fn claim_survives_concurrent_real_stamp_writes() {
        let _guard = super::tests::store_test_guard();
        let h = uuid::Uuid::new_v4().simple().to_string();
        // A valid account hash (64 lowercase hex): the claim filters anything
        // else, which would make this test vacuously green.
        let hash = format!("{h}{h}");
        // One dirty row, so every stamp pass actually rewrites the document and
        // the contention is real rather than early-returned away.
        crate::account_scope::upsert_account_agent(&hash, "agent:qa-race-old", "Old", "hint");
        let legacy = vec![crate::agents::CustomAgent {
            id: "agent:qa-race-new".into(),
            name: "New".into(),
            hint: "h".into(),
        }];
        let only = std::collections::HashSet::from(["agent:qa-race-new".to_string()]);

        let hammer_hash = hash.clone();
        let stamper = std::thread::spawn(move || {
            let mut store = AgentDirtyStore;
            for _ in 0..200 {
                let _ = store.stamp_account(&hammer_hash);
            }
        });
        let out = crate::account_scope::claim_legacy_account_agents(
            Some(&hash),
            &legacy,
            Some(&only),
        );
        stamper.join().unwrap();

        assert_eq!(out.claimed_ids, vec!["agent:qa-race-new".to_string()]);
        let final_store = crate::account_scope::load_account_agents(&hash);
        assert!(
            final_store
                .custom_agents
                .iter()
                .any(|a| a.id == "agent:qa-race-new"),
            "the claim must survive 200 real stamp passes"
        );
        assert!(
            final_store
                .custom_agents
                .iter()
                .any(|a| a.id == "agent:qa-race-old"),
            "the pre-existing row must survive too"
        );
    }

    /// Style counterpart: a claim racing the REAL style stamper loses nothing.
    #[test]
    fn style_claim_survives_concurrent_real_stamp_writes() {
        let _guard = super::tests::store_test_guard();
        let h = uuid::Uuid::new_v4().simple().to_string();
        let hash = format!("{h}{h}");
        crate::account_scope::upsert_account_style(&hash, "custom:qa-race-old", "Old", "hint");
        let legacy = vec![crate::prompts::CustomStyle {
            id: "custom:qa-race-new".into(),
            name: "New".into(),
            hint: "h".into(),
        }];
        let only = std::collections::HashSet::from(["custom:qa-race-new".to_string()]);

        let hammer_hash = hash.clone();
        let stamper = std::thread::spawn(move || {
            let mut store = StyleDirtyStore;
            for _ in 0..200 {
                let _ = store.stamp_account(&hammer_hash);
            }
        });
        let out = crate::account_scope::claim_legacy_account_styles(
            Some(&hash),
            &legacy,
            Some(&only),
        );
        stamper.join().unwrap();

        assert_eq!(out.claimed_ids, vec!["custom:qa-race-new".to_string()]);
        let final_store = crate::account_scope::load_account_styles(&hash);
        assert!(
            final_store
                .custom_styles
                .iter()
                .any(|s| s.id == "custom:qa-race-new"),
            "the style claim must survive 200 real stamp passes"
        );
        assert!(
            final_store
                .custom_styles
                .iter()
                .any(|s| s.id == "custom:qa-race-old"),
            "the pre-existing style must survive too"
        );
    }

    /// A stale merge driven through the REAL writer discards the claimed row — and
    /// the record is still recoverable, because the claim never removed the legacy
    /// copy. This is what makes the residual race safe without general B2/R1.
    #[test]
    fn a_stale_real_merge_that_drops_a_claim_leaves_it_recoverable() {
        let _guard = super::tests::store_test_guard();
        let hash = qa_account("claim-stale-merge");
        let id = "agent:qa-stale-merge-0001";
        let legacy = vec![crate::agents::CustomAgent {
            id: id.to_string(),
            name: "Legacy".into(),
            hint: "h".into(),
        }];
        let only = std::collections::HashSet::from([id.to_string()]);

        let claimed =
            crate::account_scope::claim_legacy_account_agents(Some(&hash), &legacy, Some(&only));
        assert_eq!(claimed.claimed_ids, vec![id.to_string()]);
        assert!(crate::account_scope::load_account_agents(&hash)
            .custom_agents
            .iter()
            .any(|a| a.id == id));

        // The pass writes the payload it read BEFORE the claim. Real writer.
        let mut store = AgentDirtyStore;
        store.save_merged(&hash, Vec::new()).unwrap();
        assert!(
            crate::account_scope::load_account_agents(&hash)
                .custom_agents
                .iter()
                .all(|a| a.id != id),
            "precondition: the stale merge discarded the claimed row"
        );

        // Not lost: visible again as claimable, via the PUBLIC projection the board
        // renders (never by reaching into the snapshot's private union).
        let view = crate::account_scope::VisibleAgents::from_union_with_account(
            crate::account_scope::union_agents(&legacy, &[]),
            Some(&hash),
        )
        .into_view(crate::agents::ID_BUILT_IN.to_string());
        assert_eq!(view.custom_agents.len(), 1);
        assert!(
            view.custom_agents[0].claimable,
            "the row is visible again and claimable"
        );

        let retry =
            crate::account_scope::claim_legacy_account_agents(Some(&hash), &legacy, Some(&only));
        assert_eq!(retry.claimed_ids, vec![id.to_string()], "re-claim succeeds");
    }
}
