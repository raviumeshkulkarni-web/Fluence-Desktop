// Fluence sync — quarantine list + user resolution (spec §12, §27 phase 9).
//
// The engine latches a DIVERGENT group on the local row with a reason and
// skips it every pass until the user resolves it (§12 — no auto-clear, no
// auto-delete, no GC). This module surfaces the latched rows to the settings
// UI and applies the two explicit user actions:
//
//   restore — clear the latch (`LocalStore::clear_quarantine`); the next pass
//             re-evaluates the group from facts: imports if HEALTHY
//             (placeholders are overwritten by import), re-quarantines if
//             still DIVERGENT. Offending Drive files are never touched.
//   discard — hard-delete the quarantined LOCAL row (explicit user action).
//             Drive files are never deleted by the app (§12, §20); if the
//             group is still DIVERGENT, the next pass re-creates the latched
//             placeholder so the offending file stays surfaced.
//
// Account namespace (§13): only rows stamped null or the active account are
// listed or resolvable. No transcript text is logged or persisted anywhere in
// this module — the command payload is the user's own UI.

use serde::Serialize;

use crate::dictionary::sync_store::DictionarySyncStore;
use crate::history::HistorySyncStore;
use crate::snippets::sync_store::SnippetSyncStore;
use crate::sync::engine::{LocalRow, LocalStore};
use crate::sync::scheduler::sync_settings_path;
use crate::sync::settings_store::SyncSettingsStore;
use crate::sync::wire::RecordType;

/// One latched row, serialized to the settings UI (camelCase, stable field
/// names — the frontend contract).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuarantinedEntry {
    /// Record kind: `history` | `dictionary` | `snippet` | `settings`.
    pub kind: String,
    pub uuid: String,
    /// `QuarantineReason::as_str()` — the latched reason.
    pub reason: String,
    pub sync_state: String,
    pub deleted_at: Option<i64>,
    pub created_at: i64,
    /// Display-only summary of the record content (frontend display; never
    /// logged, never persisted by sync code).
    pub title: String,
    /// True when the row is the engine's content-less §12 placeholder.
    pub placeholder: bool,
}

/// Deterministic display mapping for one latched row.
pub fn entry_from_row(row: &LocalRow) -> QuarantinedEntry {
    let kind = match row.rtype {
        RecordType::History => "history",
        RecordType::Dictionary => "dictionary",
        RecordType::Snippet => "snippet",
        RecordType::Settings => "settings",
    };
    let placeholder = match row.rtype {
        RecordType::History => row.text.is_empty() && row.duration_ms == 0,
        RecordType::Dictionary => {
            row.spoken.as_deref().unwrap_or_default().is_empty()
                && row.corrected.as_deref().unwrap_or_default().is_empty()
        }
        RecordType::Snippet => {
            row.trigger.as_deref().unwrap_or_default().is_empty()
                && row.expansion.as_deref().unwrap_or_default().is_empty()
        }
        RecordType::Settings => row.settings_key.as_deref().unwrap_or_default().is_empty(),
    };
    let title = match row.rtype {
        RecordType::History => {
            let mode = if row.mode.is_empty() {
                "history".to_string()
            } else {
                row.mode.clone()
            };
            let text = row.text.trim();
            if text.is_empty() {
                format!("{mode} record")
            } else {
                format!("{mode} · {}", truncate(text, 120))
            }
        }
        RecordType::Dictionary => {
            let spoken = row.spoken.as_deref().unwrap_or_default();
            let corrected = row.corrected.as_deref().unwrap_or_default();
            let kind = row.kind.as_deref().unwrap_or_default();
            if spoken.is_empty() && corrected.is_empty() {
                "dictionary entry".to_string()
            } else {
                format!(
                    "{} → {}{}",
                    truncate(spoken, 60),
                    truncate(corrected, 60),
                    kind_tag(kind)
                )
            }
        }
        RecordType::Snippet => {
            let trigger = row.trigger.as_deref().unwrap_or_default();
            let expansion = row.expansion.as_deref().unwrap_or_default();
            if trigger.is_empty() && expansion.is_empty() {
                "snippet".to_string()
            } else {
                format!("{} → {}", truncate(trigger, 60), truncate(expansion, 60))
            }
        }
        RecordType::Settings => {
            let key = row.settings_key.as_deref().unwrap_or_default();
            let value = row.settings_value.as_deref().unwrap_or_default();
            if key.is_empty() {
                "settings record".to_string()
            } else {
                format!("{key} = {}", truncate(value, 60))
            }
        }
    };
    QuarantinedEntry {
        kind: kind.to_string(),
        uuid: row.uuid.clone(),
        reason: row.quarantine_reason.clone().unwrap_or_default(),
        sync_state: row.sync_state.clone(),
        deleted_at: row.deleted_at,
        created_at: row.timestamp_ms,
        title,
        placeholder,
    }
}

fn kind_tag(kind: &str) -> String {
    if kind.is_empty() {
        String::new()
    } else {
        format!(" ({kind})")
    }
}

/// Char-boundary-safe truncation for display summaries.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// All latched rows across the four stores, scoped to the §13 namespace.
/// `list_rows(Some(account))` already returns null- OR account-stamped rows.
pub fn collect_quarantined(
    stores: &mut [Box<dyn LocalStore>],
    account: Option<String>,
) -> Vec<QuarantinedEntry> {
    let mut out: Vec<QuarantinedEntry> = Vec::new();
    for store in stores.iter_mut() {
        for row in store.list_rows(account.as_deref()) {
            if row.quarantine_reason.is_some() {
                out.push(entry_from_row(&row));
            }
        }
    }
    out.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.uuid.cmp(&b.uuid))
    });
    out
}

/// Build the four stores over their persisted state.
fn build_stores() -> Vec<Box<dyn LocalStore>> {
    vec![
        Box::new(HistorySyncStore::new()),
        Box::new(DictionarySyncStore::new()),
        Box::new(SnippetSyncStore::new()),
        Box::new(SyncSettingsStore::new(sync_settings_path())),
    ]
}

fn active_account() -> Option<String> {
    crate::settings::load_settings().ok()?.sync_account_key
}

/// Apply one user resolve action to a latched row (spec §12). Guards:
/// unknown kind/action, missing or non-latched row, and the §13 namespace.
pub fn resolve_in_store(
    store: &mut dyn LocalStore,
    account: Option<String>,
    uuid: &str,
    action: &str,
) -> Result<(), String> {
    let row = store
        .find_row(uuid)
        .ok_or_else(|| "no such synced record".to_string())?;
    if row.quarantine_reason.is_none() {
        return Err("record is not quarantined".to_string());
    }
    if let Some(active) = account {
        if let Some(stamp) = &row.sync_account {
            if stamp != &active {
                return Err("record belongs to another account".to_string());
            }
        }
    }
    match action {
        "restore" => store.clear_quarantine(uuid).map_err(|e| e.to_string()),
        "discard" => store.hard_delete(uuid).map_err(|e| e.to_string()),
        other => Err(format!("unknown resolve action: {other}")),
    }
}

/// Tauri: list every latched (quarantined) record visible to the active
/// account, newest first.
#[tauri::command]
pub fn sync_list_quarantined() -> Result<Vec<QuarantinedEntry>, String> {
    Ok(collect_quarantined(&mut build_stores(), active_account()))
}

/// Tauri: user resolves one quarantined record.
/// `kind` ∈ {history, dictionary, snippet, settings}; `action` ∈
/// {restore, discard}. Restore re-evaluates on the next pass; discard
/// hard-deletes the local row (Drive files are never deleted).
#[tauri::command]
pub fn sync_resolve_quarantine(uuid: String, kind: String, action: String) -> Result<(), String> {
    let account = active_account();
    let mut store: Box<dyn LocalStore> = match kind.as_str() {
        "history" => Box::new(HistorySyncStore::new()),
        "dictionary" => Box::new(DictionarySyncStore::new()),
        "snippet" => Box::new(SnippetSyncStore::new()),
        "settings" => Box::new(SyncSettingsStore::new(sync_settings_path())),
        other => return Err(format!("unknown record kind: {other}")),
    };
    resolve_in_store(&mut *store, account, &uuid, &action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::engine::{QuarantineReason, SyncError, SYNC_STATE_LOCAL};

    const UUID_A: &str = "00000000-0000-4000-8000-000000000001";

    fn latched_history(uuid: &str, text: &str, account: Option<&str>) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713456000123,
            text: text.to_string(),
            mode: "transcription".to_string(),
            duration_ms: 8400,
            provider: "groq".to_string(),
            model: None,
            language: None,
            rtype: RecordType::History,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: None,
            expansion: None,
            settings_key: None,
            settings_value: None,
            deleted_at: None,
            server_file_id: None,
            sync_account: account.map(str::to_string),
            sync_state: "quarantined".to_string(),
            quarantine_reason: Some(QuarantineReason::CorruptFile.as_str().to_string()),
        }
    }

    fn live_row(uuid: &str, rtype: RecordType) -> LocalRow {
        LocalRow {
            uuid: uuid.to_string(),
            timestamp_ms: 1713456000123,
            text: "x".to_string(),
            mode: "transcription".to_string(),
            duration_ms: 1,
            provider: "groq".to_string(),
            model: None,
            language: None,
            rtype,
            spoken: Some("foo".to_string()),
            corrected: Some("bar".to_string()),
            kind: Some("correction".to_string()),
            trigger: Some("addr".to_string()),
            expansion: Some("456 Oak Ave".to_string()),
            settings_key: Some("snippets_enabled".to_string()),
            settings_value: Some("true".to_string()),
            deleted_at: None,
            server_file_id: None,
            sync_account: None,
            sync_state: SYNC_STATE_LOCAL.to_string(),
            quarantine_reason: None,
        }
    }

    /// Minimal LocalStore for resolve-unit tests (the real stores all follow
    /// the same seam; the guard logic is what is under test).
    #[derive(Debug, Default)]
    struct TestStore {
        rows: Vec<LocalRow>,
    }

    impl TestStore {
        fn row(&self, uuid: &str) -> Option<LocalRow> {
            self.rows.iter().find(|r| r.uuid == uuid).cloned()
        }
    }

    impl LocalStore for TestStore {
        fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow> {
            let mut out: Vec<LocalRow> = self
                .rows
                .iter()
                .filter(|r| match account {
                    None => r.sync_account.is_none(),
                    Some(a) => r.sync_account.as_deref().map_or(true, |s| s == a),
                })
                .cloned()
                .collect();
            out.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            out
        }

        fn find_row(&self, uuid: &str) -> Option<LocalRow> {
            self.row(uuid)
        }

        fn import(&mut self, row: LocalRow) -> Result<(), SyncError> {
            if let Some(existing) = self.rows.iter_mut().find(|r| r.uuid == row.uuid) {
                *existing = row;
            } else {
                self.rows.push(row);
            }
            Ok(())
        }

        fn mark_tombstoned(&mut self, _uuid: &str, _deleted_at: i64) -> Result<(), SyncError> {
            Ok(())
        }

        fn set_server_file_id(&mut self, _uuid: &str, _file_id: &str) -> Result<(), SyncError> {
            Ok(())
        }

        fn set_sync_state(&mut self, _uuid: &str, _state: &str) -> Result<(), SyncError> {
            Ok(())
        }

        fn quarantine(&mut self, _uuid: &str, _reason: QuarantineReason) -> Result<(), SyncError> {
            Ok(())
        }

        fn clear_quarantine(&mut self, uuid: &str) -> Result<(), SyncError> {
            if let Some(r) = self.rows.iter_mut().find(|r| r.uuid == uuid) {
                r.quarantine_reason = None;
                r.sync_state = SYNC_STATE_LOCAL.to_string();
            }
            Ok(())
        }

        fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError> {
            self.rows.retain(|r| r.uuid != uuid);
            Ok(())
        }
    }

    // -- display mapping ----------------------------------------------------

    #[test]
    fn history_entry_maps_kind_reason_and_title() {
        let e = entry_from_row(&latched_history(UUID_A, "Meeting notes.", Some("a@x")));
        assert_eq!(e.kind, "history");
        assert_eq!(e.reason, "corrupt_file");
        assert_eq!(e.sync_state, "quarantined");
        assert_eq!(e.created_at, 1713456000123);
        assert!(e.title.contains("Meeting notes."));
        assert!(!e.placeholder);
    }

    #[test]
    fn long_history_text_is_truncated() {
        let long = "x".repeat(300);
        let e = entry_from_row(&latched_history(UUID_A, &long, None));
        assert!(e.title.chars().count() < 200, "title stays bounded");
        assert!(e.title.ends_with('…'));
    }

    #[test]
    fn placeholder_rows_are_detected_per_kind() {
        let mut ph = latched_history(UUID_A, "", None);
        ph.duration_ms = 0;
        assert!(entry_from_row(&ph).placeholder);
        assert!(!entry_from_row(&latched_history(UUID_A, "text", None)).placeholder);

        let mut dict = live_row(UUID_A, RecordType::Dictionary);
        dict.quarantine_reason = Some("collision".to_string());
        assert!(!entry_from_row(&dict).placeholder);
        dict.spoken = None;
        dict.corrected = None;
        assert!(entry_from_row(&dict).placeholder);

        let mut snip = live_row(UUID_A, RecordType::Snippet);
        snip.quarantine_reason = Some("collision".to_string());
        assert!(!entry_from_row(&snip).placeholder);
        snip.trigger = None;
        snip.expansion = None;
        assert!(entry_from_row(&snip).placeholder);

        let mut set = live_row(UUID_A, RecordType::Settings);
        set.quarantine_reason = Some("collision".to_string());
        assert!(!entry_from_row(&set).placeholder);
        set.settings_key = None;
        assert!(entry_from_row(&set).placeholder);
    }

    #[test]
    fn kind_titles_include_content() {
        let dict = entry_from_row(&live_row(UUID_A, RecordType::Dictionary));
        assert!(dict.title.contains("foo → bar (correction)"));
        let snip = entry_from_row(&live_row(UUID_A, RecordType::Snippet));
        assert!(snip.title.contains("addr → 456 Oak Ave"));
        let set = entry_from_row(&live_row(UUID_A, RecordType::Settings));
        assert!(set.title.contains("snippets_enabled = true"));
    }

    // -- resolve guards ----------------------------------------------------

    #[test]
    fn restore_clears_the_latch() {
        let mut store = TestStore::default();
        store.import(latched_history(UUID_A, "x", None));
        resolve_in_store(&mut store, None, UUID_A, "restore").unwrap();
        let r = store.row(UUID_A).unwrap();
        assert!(r.quarantine_reason.is_none());
        assert_eq!(r.sync_state, SYNC_STATE_LOCAL);
    }

    #[test]
    fn discard_hard_deletes_the_local_row() {
        let mut store = TestStore::default();
        store.import(latched_history(UUID_A, "x", None));
        resolve_in_store(&mut store, None, UUID_A, "discard").unwrap();
        assert!(store.row(UUID_A).is_none());
    }

    #[test]
    fn resolve_rejects_foreign_account_rows() {
        let mut store = TestStore::default();
        store.import(latched_history(UUID_A, "x", Some("other@x")));
        let err =
            resolve_in_store(&mut store, Some("me@x".to_string()), UUID_A, "restore").unwrap_err();
        assert!(err.contains("another account"));
        assert!(store.row(UUID_A).unwrap().quarantine_reason.is_some());
    }

    #[test]
    fn resolve_rejects_unlatched_rows_and_unknown_actions() {
        let mut store = TestStore::default();
        store.import(live_row(UUID_A, RecordType::History));
        assert!(resolve_in_store(&mut store, None, UUID_A, "restore").is_err());

        store.import(latched_history(UUID_A, "x", None));
        assert!(resolve_in_store(&mut store, None, UUID_A, "explode").is_err());
        assert!(resolve_in_store(&mut store, None, "missing-uuid", "restore").is_err());
    }

    #[test]
    fn collect_lists_only_latched_rows_scoped_to_account() {
        let mut store = TestStore::default();
        store.import(latched_history(UUID_A, "mine", None));
        store.import(live_row(
            "00000000-0000-4000-8000-0000000000bb",
            RecordType::History,
        ));
        let mut latched_foreign = latched_history(
            "00000000-0000-4000-8000-0000000000cc",
            "theirs",
            Some("other@x"),
        );
        latched_foreign.quarantine_reason = Some("collision".to_string());
        store.import(latched_foreign);

        let mut stores: Vec<Box<dyn LocalStore>> = vec![Box::new(store)];
        let listed = collect_quarantined(&mut stores, Some("me@x".to_string()));
        assert_eq!(listed.len(), 1, "foreign latched rows stay invisible (§13)");
        assert_eq!(listed[0].uuid, UUID_A);

        let mut stores: Vec<Box<dyn LocalStore>> = vec![Box::new(TestStore::default())];
        assert!(collect_quarantined(&mut stores, None).is_empty());
    }
}
