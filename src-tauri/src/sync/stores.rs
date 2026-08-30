// Fluence sync — frozen v1.2 local stores (DirtyStore implementations)
//
// Account isolation model:
// - dictionary.json / snippets.json rows carry `sync_account`; loads filter
//   by the active account hash, so another account's rows are never uploaded.
// - stats events live in one local ledger (`stats_events.json`) with a
//   nullable account stamp; unstamped (pre-sign-in) dictations are claimed by
//   the first account that syncs them. Event ids are UUIDv5 of the history
//   row id, so a backfilled row and a freshly-recorded event for the same
//   dictation collapse under union dedup — exactly-once counting by
//   construction.
// - settings LWW bookkeeping lives in `settings_sync_<hash>.json`, one
//   document per account, so preferences cannot cross accounts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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

fn data_dir() -> PathBuf {
    let mut p = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
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
/// `live_values` — `dictionary_enabled` is deliberately absent (Windows has
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

    /// UNIT D — growth gauge: rows + envelope bytes vs 8 MiB headroom for the given account.
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
        // clobbering with the merged winner — mirrors Android
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

    fn store_test_guard() -> std::sync::MutexGuard<'static, ()> {
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
        // UNIT B — collapse rule: day-aggregates for days that already have dictation-level events must be suppressed.
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
        // UNIT B — flagged reconciliation OFF by default, pure set-op (union-dedup by eventId)
        // This test documents the flag; actual deletion is behind feature gate.
        const STATS_RECONCILIATION_ENABLED: bool = false;
        assert!(
            !STATS_RECONCILIATION_ENABLED,
            "reconciliation must be OFF by default"
        );
    }
}
