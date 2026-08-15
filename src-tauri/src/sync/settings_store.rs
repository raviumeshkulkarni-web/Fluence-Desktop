// Fluence sync — settings record store (spec §30.3, §30.5).
//
// Keyed settings (currently `snippets_enabled`) sync as `settings` records.
// This store is the LocalStore seam for the settings kind: an in-memory
// registry of rows, persisted as JSON at `Fluence/sync-settings.json`. The
// §30.3 semantics live here: a toggle tombstones the live row and creates a
// new UUID row; a key-collision (same key, different value, both live) latches
// the incoming row with `collision` so the local value never silently loses.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::sync::engine::{
    LocalRow, LocalStore, QuarantineReason, SyncError, SYNC_STATE_DIRTY, SYNC_STATE_LOCAL,
    SYNC_STATE_QUARANTINED,
};
use crate::sync::wire::RecordType;

pub const KEY_SNIPPETS_ENABLED: &str = "snippets_enabled";

/// One settings row — the §6-table shadow for the settings kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettingsRow {
    pub uuid: String,
    pub created_at: i64,
    pub key: String,
    pub value: String,
    pub deleted_at: Option<i64>,
    pub server_file_id: Option<String>,
    pub sync_account: Option<String>,
    pub sync_state: String,
    pub quarantine_reason: Option<String>,
}

/// In-memory settings registry with JSON persistence. `path = None` keeps
/// the store memory-only (used by tests); the app passes the canonical
/// `Fluence/sync-settings.json` path.
#[derive(Debug)]
pub struct SyncSettingsStore {
    rows: Vec<SyncSettingsRow>,
    path: Option<PathBuf>,
}

impl SyncSettingsStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        let rows = match &path {
            Some(p) if p.exists() => fs::read_to_string(p)
                .ok()
                .and_then(|data| serde_json::from_str::<Vec<SyncSettingsRow>>(&data).ok())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        Self { rows, path }
    }

    pub fn rows(&self) -> &[SyncSettingsRow] {
        &self.rows
    }

    /// §30.3 toggle: tombstone the live row for `key`, then create a fresh
    /// UUID row with `value` (unstamped, §13). Returns the new row's UUID.
    pub fn toggle(&mut self, key: &str, value: &str) -> String {
        let now = Utc::now().timestamp_millis();
        let live_uuid = self.live_row(key).map(|r| r.uuid.clone());
        if let Some(uuid) = live_uuid {
            if let Some(row) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
                row.deleted_at = Some(now);
                row.sync_state = SYNC_STATE_DIRTY.to_string();
            }
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        // The fresh row is unstamped (§13): `sync_account` is an import
        // marker, so local rows keep it NULL and match any account.
        self.rows.push(SyncSettingsRow {
            uuid: uuid.clone(),
            created_at: now,
            key: key.to_string(),
            value: value.to_string(),
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        });
        self.save();
        uuid
    }

    /// The newest live (untombstoned, unlatched) row for `key`.
    pub fn live_row(&self, key: &str) -> Option<&SyncSettingsRow> {
        self.rows
            .iter()
            .filter(|r| r.key == key && r.deleted_at.is_none() && r.quarantine_reason.is_none())
            .max_by_key(|r| r.created_at)
    }

    pub fn live_value(&self, key: &str) -> Option<&str> {
        self.live_row(key).map(|r| r.value.as_str())
    }

    pub fn live_enabled(&self) -> Option<bool> {
        self.live_value(KEY_SNIPPETS_ENABLED).map(|v| v == "true")
    }

    /// Apply the synced `snippets_enabled` value to the local feature toggle
    /// through the caller-provided sink (Phase 7 wires this to
    /// `snippets::set_snippets_enabled`). No-op when the key has no live row.
    pub fn mirror_enabled(&self, sink: &mut dyn FnMut(bool)) {
        if let Some(enabled) = self.live_enabled() {
            sink(enabled);
        }
    }

    fn save(&self) {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(data) = serde_json::to_string_pretty(&self.rows) {
                let _ = fs::write(path, data);
            }
        }
    }

    /// Latch the incoming live row when another live row holds the same key
    /// with a different value (§30.5 `settings_toggle_quarantines_on_...`).
    fn latch_key_collision(&mut self, row: &mut SyncSettingsRow) {
        if row.deleted_at.is_some() || row.quarantine_reason.is_some() {
            return;
        }
        let collides = self.rows.iter().any(|r| {
            r.uuid != row.uuid
                && r.key == row.key
                && r.deleted_at.is_none()
                && r.quarantine_reason.is_none()
                && r.value != row.value
        });
        if collides {
            row.quarantine_reason = Some(QuarantineReason::Collision.as_str().to_string());
            row.sync_state = SYNC_STATE_QUARANTINED.to_string();
        }
    }
}

fn to_local(row: &SyncSettingsRow) -> LocalRow {
    LocalRow {
        uuid: row.uuid.clone(),
        timestamp_ms: row.created_at,
        text: String::new(),
        mode: String::new(),
        duration_ms: 0,
        provider: String::new(),
        model: None,
        language: None,
        rtype: RecordType::Settings,
        spoken: None,
        corrected: None,
        kind: None,
        trigger: None,
        expansion: None,
        settings_key: Some(row.key.clone()),
        settings_value: Some(row.value.clone()),
        deleted_at: row.deleted_at,
        server_file_id: row.server_file_id.clone(),
        sync_account: row.sync_account.clone(),
        sync_state: row.sync_state.clone(),
        quarantine_reason: row.quarantine_reason.clone(),
    }
}

fn from_local(row: LocalRow) -> Option<SyncSettingsRow> {
    if row.rtype != RecordType::Settings {
        return None;
    }
    Some(SyncSettingsRow {
        uuid: row.uuid,
        created_at: row.timestamp_ms,
        key: row.settings_key.unwrap_or_default(),
        value: row.settings_value.unwrap_or_default(),
        deleted_at: row.deleted_at,
        server_file_id: row.server_file_id,
        sync_account: row.sync_account,
        sync_state: row.sync_state,
        quarantine_reason: row.quarantine_reason,
    })
}

impl LocalStore for SyncSettingsStore {
    fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow> {
        let mut out: Vec<LocalRow> = self
            .rows
            .iter()
            .filter(|r| match account {
                None => r.sync_account.is_none(),
                Some(a) => r.sync_account.as_deref().map_or(true, |s| s == a),
            })
            .map(to_local)
            .collect();
        out.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        out
    }

    fn find_row(&self, uuid: &str) -> Option<LocalRow> {
        self.rows.iter().find(|r| r.uuid == uuid).map(to_local)
    }

    fn import(&mut self, row: LocalRow) -> Result<(), SyncError> {
        let Some(mut incoming) = from_local(row) else {
            return Ok(()); // other kinds never reach this store
        };
        self.latch_key_collision(&mut incoming);
        if let Some(existing) = self.rows.iter_mut().find(|r| r.uuid == incoming.uuid) {
            *existing = incoming;
        } else {
            self.rows.push(incoming);
        }
        self.save();
        Ok(())
    }

    fn mark_tombstoned(&mut self, uuid: &str, deleted_at: i64) -> Result<(), SyncError> {
        if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
            r.deleted_at = Some(deleted_at);
            r.sync_state = SYNC_STATE_DIRTY.to_string();
        }
        self.save();
        Ok(())
    }

    fn set_server_file_id(&mut self, uuid: &str, file_id: &str) -> Result<(), SyncError> {
        if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
            r.server_file_id = Some(file_id.to_string());
        }
        self.save();
        Ok(())
    }

    fn set_sync_state(&mut self, uuid: &str, state: &str) -> Result<(), SyncError> {
        if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
            r.sync_state = state.to_string();
        }
        self.save();
        Ok(())
    }

    fn quarantine(&mut self, uuid: &str, reason: QuarantineReason) -> Result<(), SyncError> {
        if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
            r.quarantine_reason = Some(reason.as_str().to_string());
            r.sync_state = SYNC_STATE_QUARANTINED.to_string();
        }
        self.save();
        Ok(())
    }

    fn clear_quarantine(&mut self, uuid: &str) -> Result<(), SyncError> {
        if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
            r.quarantine_reason = None;
            r.sync_state = SYNC_STATE_LOCAL.to_string();
        }
        self.save();
        Ok(())
    }

    fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError> {
        self.rows.retain(|r| r.uuid != uuid);
        self.save();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::engine::{SyncOutcome, TokenProvider};
    use crate::sync::wire::WireRecord;

    fn memory_store() -> SyncSettingsStore {
        SyncSettingsStore::new(None)
    }

    fn settings_row(uuid: &str, key: &str, value: &str) -> LocalRow {
        let mut row = LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713471000123,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            rtype: RecordType::Settings,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: None,
            expansion: None,
            settings_key: Some(key.to_string()),
            settings_value: Some(value.to_string()),
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        };
        row.timestamp_ms = 1713471000123 + row.uuid.as_bytes()[0] as i64;
        row
    }

    fn wire_of(row: &LocalRow) -> WireRecord {
        crate::sync::wire::parse(row.to_wire().to_json().as_bytes(), &row.uuid)
            .expect("row serializes to a valid settings record")
    }

    #[test]
    fn enabled_toggle_roundtrips() {
        let mut store = memory_store();
        let u1 = store.toggle(KEY_SNIPPETS_ENABLED, "true");
        assert_eq!(store.live_enabled(), Some(true));
        assert_eq!(store.live_value(KEY_SNIPPETS_ENABLED), Some("true"));

        // Toggle off: old row tombstoned, fresh UUID row with the new value.
        let u2 = store.toggle(KEY_SNIPPETS_ENABLED, "false");
        assert_ne!(u1, u2);
        assert_eq!(store.live_enabled(), Some(false));

        let old = store
            .rows()
            .iter()
            .find(|r| r.uuid == u1)
            .expect("old row kept as tombstone");
        assert!(old.deleted_at.is_some());
        assert_eq!(old.value, "true");

        // §30 wire roundtrip: the new row is a valid settings record.
        let fresh = store.rows().iter().find(|r| r.uuid == u2).unwrap();
        let rec = wire_of(&to_local(fresh));
        assert_eq!(rec.rtype, RecordType::Settings);
        assert_eq!(rec.settings_key.as_deref(), Some(KEY_SNIPPETS_ENABLED));
        assert_eq!(rec.settings_value.as_deref(), Some("false"));

        // A re-toggled row's UUID is fresh again; roundtrip stays lossless.
        let u3 = store.toggle(KEY_SNIPPETS_ENABLED, "true");
        assert_ne!(u3, u2);
        assert_eq!(store.live_enabled(), Some(true));
    }

    #[test]
    fn settings_toggle_quarantines_on_divergence() {
        // Two live rows for the same key with different values: the incoming
        // row is latched `collision`; the local value is never silently lost.
        let mut store = memory_store();
        let u1 = store.toggle(KEY_SNIPPETS_ENABLED, "true");

        let diverged = settings_row(
            "00000000-0000-4000-8000-0000000000dd",
            KEY_SNIPPETS_ENABLED,
            "false",
        );
        store.import(diverged).expect("import succeeds");
        store
            .import(settings_row(
                "00000000-0000-4000-8000-0000000000ee",
                KEY_SNIPPETS_ENABLED,
                "false",
            ))
            .expect("import succeeds");

        let incoming = store
            .rows()
            .iter()
            .find(|r| r.uuid == "00000000-0000-4000-8000-0000000000ee")
            .expect("incoming row present");
        assert_eq!(
            incoming.quarantine_reason.as_deref(),
            Some("collision"),
            "divergent live value must be latched, not silently adopted"
        );
        assert_eq!(incoming.sync_state, SYNC_STATE_QUARANTINED);
        assert_eq!(store.live_enabled(), Some(true), "local value wins");
        assert_eq!(store.live_row(KEY_SNIPPETS_ENABLED).unwrap().uuid, u1);

        // A tombstone for the same key is never a collision.
        let mut tomb = settings_row(
            "00000000-0000-4000-8000-0000000000ff",
            KEY_SNIPPETS_ENABLED,
            "true",
        );
        tomb.deleted_at = Some(1713472000123);
        tomb.sync_state = SYNC_STATE_DIRTY.to_string();
        store.import(tomb).expect("tombstone imports");
        let tomb_row = store
            .rows()
            .iter()
            .find(|r| r.uuid == "00000000-0000-4000-8000-0000000000ff")
            .expect("tombstone row present");
        assert!(tomb_row.quarantine_reason.is_none());
    }

    #[test]
    fn tombstone_import_never_collides() {
        let mut store = memory_store();
        store.toggle(KEY_SNIPPETS_ENABLED, "true");
        let mut tomb = settings_row(
            "00000000-0000-4000-8000-0000000000aa",
            KEY_SNIPPETS_ENABLED,
            "false",
        );
        tomb.deleted_at = Some(1713472000123);
        store.import(tomb).expect("tombstone imports");
        let incoming = store
            .rows()
            .iter()
            .find(|r| r.uuid == "00000000-0000-4000-8000-0000000000aa")
            .unwrap();
        assert!(incoming.quarantine_reason.is_none());
        assert_eq!(store.live_enabled(), Some(true));
    }

    #[test]
    fn import_is_idempotent_and_persists() {
        let path = std::env::temp_dir().join(format!(
            "fluence-sync-settings-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut store = SyncSettingsStore::new(Some(path.clone()));
        store.toggle(KEY_SNIPPETS_ENABLED, "true");
        let live = store.live_row(KEY_SNIPPETS_ENABLED).unwrap().clone();
        let as_local = to_local(&live);

        // Re-import of the same row is an idempotent upsert (one row only).
        store.import(as_local.clone()).expect("upsert");
        store.import(as_local).expect("upsert");
        assert_eq!(store.rows().len(), 1);

        // A fresh instance from the same path sees the persisted row.
        let reloaded = SyncSettingsStore::new(Some(path.clone()));
        assert_eq!(reloaded.live_enabled(), Some(true));
        assert_eq!(reloaded.rows().len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn mirror_enabled_applies_live_value() {
        let mut store = memory_store();
        let mut applied: Vec<bool> = Vec::new();
        store.mirror_enabled(&mut |v| applied.push(v));
        assert!(applied.is_empty(), "no live row → nothing to mirror");

        store.toggle(KEY_SNIPPETS_ENABLED, "false");
        store.mirror_enabled(&mut |v| applied.push(v));
        assert_eq!(applied, vec![false]);
    }

    #[test]
    fn engine_pass_round_trips_a_toggle() {
        // Store + engine integration: toggle → engine creates the record on
        // Drive and the store row converges to clean.
        use crate::sync::engine::{self, DriveStore};
        use crate::sync::wire::RecordType;

        struct FakeDrive {
            files: Vec<(String, WireRecord)>,
        }
        impl DriveStore for FakeDrive {
            fn find_or_create_folder(&mut self) -> Result<(), SyncError> {
                Ok(())
            }
            fn list_files(&mut self) -> Result<Vec<engine::FileMeta>, SyncError> {
                Ok(self
                    .files
                    .iter()
                    .map(|(id, r)| engine::FileMeta {
                        file_id: id.clone(),
                        name: format!("{}.json", r.id),
                    })
                    .collect())
            }
            fn get_content(&mut self, file_id: &str) -> Result<Option<Vec<u8>>, SyncError> {
                Ok(self
                    .files
                    .iter()
                    .find(|(id, _)| id == file_id)
                    .map(|(_, r)| r.to_json().into_bytes()))
            }
            fn create_file(
                &mut self,
                _name: &str,
                record: &WireRecord,
            ) -> Result<String, SyncError> {
                let id = format!("F{}", self.files.len() + 1);
                self.files.push((id.clone(), record.clone()));
                Ok(id)
            }
            fn update_content(
                &mut self,
                file_id: &str,
                record: &WireRecord,
            ) -> Result<(), SyncError> {
                if let Some((_, r)) = self.files.iter_mut().find(|(id, _)| id == file_id) {
                    *r = record.clone();
                }
                Ok(())
            }
        }
        struct ValidToken;
        impl TokenProvider for ValidToken {
            fn has_valid_token(&mut self) -> bool {
                true
            }
        }

        let mut drive = FakeDrive { files: vec![] };
        let mut store = memory_store();
        store.toggle(KEY_SNIPPETS_ENABLED, "true");

        let o = engine::run(
            RecordType::Settings,
            Some("account-a"),
            &mut store,
            &mut drive,
            &mut ValidToken,
        )
        .expect("settings pass succeeds");
        assert_eq!(
            o,
            SyncOutcome {
                created: 1,
                ..SyncOutcome::default()
            }
        );
        let live = store.live_row(KEY_SNIPPETS_ENABLED).unwrap();
        assert!(live.server_file_id.is_some());
        assert_eq!(live.sync_state, "clean");
        assert_eq!(drive.files.len(), 1);
        assert_eq!(
            drive.files[0].1.settings_key.as_deref(),
            Some(KEY_SNIPPETS_ENABLED)
        );
        assert_eq!(drive.files[0].1.settings_value.as_deref(), Some("true"));
    }
}
