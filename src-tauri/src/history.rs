// Fluence Windows — Transcription History module
// SQLite database in app data directory for storing past transcription sessions.

use anyhow::Result;
use chrono::{Datelike, TimeZone, Utc};
use dirs::data_local_dir;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::Emitter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: String,
    pub text: String,
    pub mode: String, // "transcription" | "agent"
    pub duration_ms: u64,
    pub provider: String,
    pub char_count: usize,
    pub model: Option<String>,
    pub language: Option<String>,
    // Sync (§29#3b): ownership indicator for the UI — foreign-stamped rows
    // (sync_account set and different from the active account) are read-only.
    pub sync_account: Option<String>,
    pub quarantine_reason: Option<String>,
    pub sync_state: Option<String>,
}

static DB: Mutex<Option<Connection>> = Mutex::new(None);

// False when the v0→v1 migration failed: reads keep serving from the old
// schema and sync stays disabled ("serve with sync disabled" seam).
pub(crate) static MIGRATION_OK: AtomicBool = AtomicBool::new(true);

// Serializes local delete/clear mutations against the sync pass (Phase 7
// seam; no sync code touches it yet).
static LOCAL_MUTATION_MUTEX: Mutex<()> = Mutex::new(());

fn db_path() -> PathBuf {
    let mut path = data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("Fluence");
    path.push("history.db");
    path
}

pub fn init_db() -> Result<()> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(&path)?;
    run_migration(&conn)?;

    let mut db = DB
        .lock()
        .map_err(|e| anyhow::anyhow!("DB lock poisoned: {}", e))?;
    *db = Some(conn);
    Ok(())
}

// Creates the schema (full 15-column layout) and migrates a legacy DB
// (PRAGMA user_version 0) through v1 to v2. A migration failure rolls back,
// logs, sets MIGRATION_OK = false and returns Ok so reads keep serving from
// the old schema — the "serve with sync disabled" seam.
fn run_migration(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS history (
            id              TEXT PRIMARY KEY,
            timestamp       TEXT NOT NULL,
            text            TEXT NOT NULL,
            mode            TEXT NOT NULL DEFAULT 'transcription',
            duration_ms     INTEGER NOT NULL DEFAULT 0,
            provider        TEXT NOT NULL DEFAULT '',
            char_count      INTEGER NOT NULL DEFAULT 0,
            timestamp_ms    INTEGER NOT NULL DEFAULT 0,
            model           TEXT,
            language        TEXT,
            deleted_at      INTEGER,
            sync_state      TEXT NOT NULL DEFAULT 'local',
            server_file_id  TEXT,
            sync_account    TEXT,
            quarantine_reason TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(timestamp DESC);",
    )?;

    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    // v0 → v1 (legacy DB without the sync columns). A fresh full-schema DB
    // needs no structural migration. A failed migration rolls back, logs, and
    // returns so reads keep serving from the old schema — the "serve with
    // sync disabled" seam. v1 → v2 must be skipped when v0 → v1 failed.
    if user_version < 1 {
        let has_timestamp_ms = {
            let mut stmt = conn.prepare("PRAGMA table_info(history)")?;
            let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut found = false;
            for col in cols.flatten() {
                if col == "timestamp_ms" {
                    found = true;
                    break;
                }
            }
            found
        };

        if !has_timestamp_ms {
            if let Err(e) = migrate_v0_to_v1(conn) {
                log::error!(
                    "History DB migration failed; serving with sync disabled: {}",
                    e
                );
                MIGRATION_OK.store(false, Ordering::SeqCst);
                return Ok(());
            }
        }
    }

    // v1 → v2: backfill the integer millisecond column from the RFC3339
    // `timestamp` string where the v1 write path left it at 0 (pre-sync
    // builds inserted rows with the DEFAULT), and un-latch previously
    // quarantined corrupt-file rows that now carry valid millis and real
    // content so the sync engine's auto-repair heals their Drive files on the
    // next pass. Content-less placeholders and rows awaiting user resolution
    // stay latched.
    if user_version < 2 {
        if let Err(e) = migrate_v1_to_v2(conn) {
            log::error!("History DB v2 migration failed: {}", e);
            MIGRATION_OK.store(false, Ordering::SeqCst);
        }
    }
    Ok(())
}

fn migrate_v0_to_v1(conn: &Connection) -> Result<()> {
    // The connection is exclusively held (behind the DB mutex), so the
    // &self transaction API is safe here.
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE history ADD COLUMN timestamp_ms INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE history ADD COLUMN model TEXT;
         ALTER TABLE history ADD COLUMN language TEXT;
         ALTER TABLE history ADD COLUMN deleted_at INTEGER;
         ALTER TABLE history ADD COLUMN sync_state TEXT NOT NULL DEFAULT 'local';
         ALTER TABLE history ADD COLUMN server_file_id TEXT;
         ALTER TABLE history ADD COLUMN sync_account TEXT;
         ALTER TABLE history ADD COLUMN quarantine_reason TEXT;",
    )?;

    // Backfill timestamp_ms from the RFC3339 timestamp (best effort; failure → 0).
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT id, timestamp FROM history")?;
        let iter = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        iter.collect::<Result<Vec<_>, _>>()?
    };
    for (id, ts) in rows {
        let ms = chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0);
        tx.execute(
            "UPDATE history SET timestamp_ms = ?1 WHERE id = ?2",
            params![ms, id],
        )?;
    }

    tx.execute_batch("PRAGMA user_version = 1")?;
    tx.commit()?;
    Ok(())
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    // The connection is exclusively held (behind the DB mutex), so the
    // &self transaction API is safe here.
    let tx = conn.unchecked_transaction()?;

    // Backfill timestamp_ms from the RFC3339 timestamp for rows the v1 write
    // path left at the DEFAULT 0 (best effort; unparseable → stays 0).
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT id, timestamp FROM history WHERE timestamp_ms = 0")?;
        let iter = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        iter.collect::<Result<Vec<_>, _>>()?
    };
    for (id, ts) in rows {
        let ms = chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0);
        tx.execute(
            "UPDATE history SET timestamp_ms = ?1 WHERE id = ?2",
            params![ms, id],
        )?;
    }

    // Un-latch corrupt-file rows that now carry valid millis and real content
    // so the sync engine can auto-repair their Drive files on the next pass.
    // Content-less placeholders and records awaiting user resolution stay
    // latched; non-repairable corrupt files are simply re-latched by the next
    // sync pass (the auto-repair rule only patches self-inflicted bad
    // timestamps/modes owned by the local row).
    tx.execute_batch(
        "UPDATE history
         SET quarantine_reason = NULL,
             sync_state = 'local'
         WHERE quarantine_reason = 'corrupt_file'
           AND text <> ''
           AND timestamp_ms > 0",
    )?;

    tx.execute_batch("PRAGMA user_version = 2")?;
    tx.commit()?;
    Ok(())
}

fn with_db<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T>,
{
    // Try to get connection; if None, initialize first (outside the lock scope)
    let needs_init = {
        let db = DB
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock poisoned: {}", e))?;
        db.is_none()
    };
    if needs_init {
        init_db()?;
    }
    let db = DB
        .lock()
        .map_err(|e| anyhow::anyhow!("DB lock poisoned: {}", e))?;
    match db.as_ref() {
        Some(conn) => f(conn),
        None => Err(anyhow::anyhow!("DB not initialized after init_db()")),
    }
}

pub fn add_history_entry(
    text: &str,
    mode: &str,
    duration_ms: u64,
    provider: &str,
) -> Result<HistoryEntry> {
    // Canonical mode values only; the write seam must never emit a record the
    // sync parser rejects (BadMode → corrupt_file quarantine).
    let mode = if mode == "transcription" || mode == "agent" {
        mode
    } else {
        log::warn!(
            "history entry with non-canonical mode '{}' coerced to 'transcription'",
            mode
        );
        "transcription"
    };
    let now = Utc::now();
    let entry = HistoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: now.to_rfc3339(),
        text: text.to_string(),
        mode: mode.to_string(),
        duration_ms,
        provider: provider.to_string(),
        char_count: text.chars().count(),
        model: None,
        language: None,
        sync_account: None,
        quarantine_reason: None,
        sync_state: Some(crate::sync::engine::SYNC_STATE_LOCAL.to_string()),
    };

    with_db(|conn| {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.id,
                entry.timestamp,
                entry.text,
                entry.mode,
                entry.duration_ms as i64,
                entry.provider,
                entry.char_count as i64,
                now.timestamp_millis(),
            ],
        )?;
        Ok(())
    })?;

    Ok(entry)
}

// Tauri commands

#[tauri::command]
pub fn get_history(page: u32, search_query: Option<String>) -> Result<Vec<HistoryEntry>, String> {
    let page_size = 50i64;
    let offset = (page as i64) * page_size;

    with_db(|conn| {
        let query = search_query.as_deref().unwrap_or("").trim().to_string();
        let rows: Result<Vec<HistoryEntry>, _> = if query.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, model, language, sync_account, quarantine_reason, sync_state
                 FROM history WHERE deleted_at IS NULL AND quarantine_reason IS NULL
                 ORDER BY timestamp_ms DESC LIMIT ?1 OFFSET ?2",
            )?;
            let res = stmt.query_map(params![page_size, offset], map_row)
                .map_err(|e| anyhow::anyhow!(e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow::anyhow!(e));
            res
        } else {
            // Escape LIKE wildcards in user search query
            let escaped_query = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let pattern = format!("%{}%", escaped_query);
            let mut stmt = conn.prepare(
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, model, language, sync_account, quarantine_reason, sync_state
                 FROM history WHERE text LIKE ?1 ESCAPE '\\' AND deleted_at IS NULL AND quarantine_reason IS NULL
                 ORDER BY timestamp_ms DESC LIMIT ?2 OFFSET ?3",
            )?;
            let res = stmt.query_map(params![pattern, page_size, offset], map_row)
                .map_err(|e| anyhow::anyhow!(e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow::anyhow!(e));
            res
        };
        rows
    })
    .map_err(|e| e.to_string())
}

fn map_row(row: &rusqlite::Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        text: row.get(2)?,
        mode: row.get(3)?,
        duration_ms: row.get::<_, i64>(4)? as u64,
        provider: row.get(5)?,
        char_count: row.get::<_, i64>(6)? as usize,
        model: row.get(8)?,
        language: row.get(9)?,
        sync_account: row.get(10)?,
        quarantine_reason: row.get(11)?,
        sync_state: row.get(12)?,
    })
}

#[tauri::command]
pub fn save_history_entry(
    app: tauri::AppHandle,
    text: String,
    mode: String,
    duration_ms: u64,
    provider: String,
) -> Result<HistoryEntry, String> {
    let entry =
        add_history_entry(&text, &mode, duration_ms, &provider).map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(entry)
}

#[tauri::command]
pub fn delete_history_entry(app: tauri::AppHandle, id: String) -> Result<(), String> {
    delete_history_by_id(&id).map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(())
}

// §14 delete rule: never-uploaded rows (server_file_id NULL) are hard-deleted;
// uploaded rows are tombstoned (deleted_at set, sync_state 'dirty');
// already-tombstoned rows are left untouched. Foreign-stamped rows
// (sync_account set to a different account) are never touched — §29#3b marks
// them read-only. Serialized through LOCAL_MUTATION_MUTEX.
pub(crate) fn delete_history_by_id(id: &str) -> Result<()> {
    let _guard = LOCAL_MUTATION_MUTEX
        .lock()
        .map_err(|e| anyhow::anyhow!("local mutation lock poisoned: {}", e))?;
    let active_account = active_account_key();
    with_db(|conn| delete_history_row(conn, id, active_account.as_deref()))
}

fn active_account_key() -> Option<String> {
    crate::settings::load_settings()
        .ok()
        .and_then(|s| s.sync_account_key)
}

fn delete_history_row(conn: &Connection, id: &str, active_account: Option<&str>) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let row: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT server_file_id, sync_account FROM history WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| anyhow::anyhow!(e))?;
    let Some((server_file_id, sync_account)) = row else {
        return Ok(()); // nothing to delete
    };
    if let Some(account) = sync_account {
        if active_account != Some(account.as_str()) {
            return Err(anyhow::anyhow!(
                "this entry belongs to sync account '{}' and is read-only here",
                account
            ));
        }
    }
    match server_file_id {
        Some(_) => {
            tx.execute(
                "UPDATE history SET deleted_at = ?1, sync_state = 'dirty'
                 WHERE id = ?2 AND deleted_at IS NULL",
                params![Utc::now().timestamp_millis(), id],
            )?;
        }
        None => {
            tx.execute("DELETE FROM history WHERE id = ?1", params![id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[tauri::command]
pub fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    clear_all_history().map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(())
}

// §14 clear rule: one transaction — hard-delete never-uploaded rows, tombstone
// the rest. Foreign-stamped rows (other accounts) are never touched (§29#3b).
// Serialized through LOCAL_MUTATION_MUTEX.
pub(crate) fn clear_all_history() -> Result<()> {
    let _guard = LOCAL_MUTATION_MUTEX
        .lock()
        .map_err(|e| anyhow::anyhow!("local mutation lock poisoned: {}", e))?;
    let active_account = active_account_key();
    with_db(|conn| clear_history_rows(conn, active_account.as_deref()))
}

fn clear_history_rows(conn: &Connection, active_account: Option<&str>) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM history WHERE server_file_id IS NULL
         AND (sync_account IS NULL OR sync_account = ?1)",
        params![active_account],
    )?;
    tx.execute(
        "UPDATE history SET deleted_at = ?1, sync_state = 'dirty'
         WHERE server_file_id IS NOT NULL AND deleted_at IS NULL
           AND (sync_account IS NULL OR sync_account = ?2)",
        params![Utc::now().timestamp_millis(), active_account],
    )?;
    tx.commit()?;
    Ok(())
}

// UTC midnight on the Monday of the week containing `now_ms`. All statistics
// bucketing uses this boundary so every device computes identical buckets.
fn utc_week_start_ms(now_ms: i64) -> i64 {
    let now = Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .expect("valid millis");
    let weekday = now.weekday().num_days_from_monday(); // 0 = Monday
    let monday = now.date_naive() - chrono::Duration::days(weekday as i64);
    Utc.from_utc_datetime(&monday.and_hms_opt(0, 0, 0).unwrap())
        .timestamp_millis()
}

// UTC midnight on the 1st of the month containing `now_ms`.
fn utc_month_start_ms(now_ms: i64) -> i64 {
    let now = Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .expect("valid millis");
    let first = now.date_naive().with_day(1).expect("day 1 always valid");
    Utc.from_utc_datetime(&first.and_hms_opt(0, 0, 0).unwrap())
        .timestamp_millis()
}

// Word count is defined as the number of whitespace-separated tokens, matching
// the platform contract (identical formula on Android and Windows).
fn word_count(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryStats {
    pub total_entries: i64,
    pub total_chars: i64,
    pub total_duration_ms: i64,
    pub total_words: i64,
    pub weekly_count: i64,
    pub weekly_duration_ms: i64,
    pub weekly_words: i64,
    pub monthly_count: i64,
    pub monthly_words: i64,
    pub week_start_ms: i64,
    pub month_start_ms: i64,
}

// Pure, deterministic computation over a set of rows. `now_ms` is injected so
// tests can pin the week/month boundaries. All aggregates are derived from the
// synced record fields only (text, timestamp_ms, duration_ms); the local
// char_count column is deliberately NOT used so both platforms agree.
fn compute_stats(rows: &[(i64, i64, String)], now_ms: i64) -> HistoryStats {
    let week_start = utc_week_start_ms(now_ms);
    let month_start = utc_month_start_ms(now_ms);

    let mut stats = HistoryStats {
        total_entries: 0,
        total_chars: 0,
        total_duration_ms: 0,
        total_words: 0,
        weekly_count: 0,
        weekly_duration_ms: 0,
        weekly_words: 0,
        monthly_count: 0,
        monthly_words: 0,
        week_start_ms: week_start,
        month_start_ms: month_start,
    };

    for (timestamp_ms, duration_ms, text) in rows {
        let words = word_count(text);
        let chars = text.chars().count() as i64;
        stats.total_entries += 1;
        stats.total_chars += chars;
        stats.total_duration_ms += duration_ms;
        stats.total_words += words;
        if *timestamp_ms >= week_start {
            stats.weekly_count += 1;
            stats.weekly_duration_ms += duration_ms;
            stats.weekly_words += words;
        }
        if *timestamp_ms >= month_start {
            stats.monthly_count += 1;
            stats.monthly_words += words;
        }
    }

    stats
}

#[tauri::command]
pub fn get_history_stats() -> Result<HistoryStats, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT timestamp_ms, duration_ms, text FROM history WHERE deleted_at IS NULL AND quarantine_reason IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(compute_stats(&rows, Utc::now().timestamp_millis()))
    })
    .map_err(|e| e.to_string())
}

// Weekly activity rows. The boundary is an RFC3339 instant (UTC); rows are
// matched on the integer timestamp_ms column so the comparison is exact and
// immune to the fractional-second formatting of the timestamp string.
fn weekly_activity(conn: &Connection, week_start_ms: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT timestamp FROM history WHERE timestamp_ms >= ?1 AND deleted_at IS NULL AND quarantine_reason IS NULL",
    )?;
    let rows = stmt.query_map(params![week_start_ms], |r| r.get::<_, String>(0))?;
    let mut timestamps = Vec::new();
    for ts in rows.flatten() {
        timestamps.push(ts);
    }
    Ok(timestamps)
}

#[tauri::command]
pub fn get_weekly_activity(start_of_week_utc: String) -> Result<Vec<String>, String> {
    let week_start_ms = chrono::DateTime::parse_from_rfc3339(&start_of_week_utc)
        .map_err(|e| format!("invalid start_of_week_utc '{}': {}", start_of_week_utc, e))?
        .timestamp_millis();
    with_db(|conn| Ok(weekly_activity(conn, week_start_ms)?)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// §30 sync — HistorySyncStore (LocalStore seam for the history kind, §27).
// Rows keep the 15-column v1 schema; the store filters the account namespace
// (§13), commits each mutation atomically (one statement per call), and
// serializes against user delete/clear via LOCAL_MUTATION_MUTEX.
// ---------------------------------------------------------------------------

use crate::sync::engine::{
    LocalRow, LocalStore, QuarantineReason, SyncError, SYNC_STATE_DIRTY, SYNC_STATE_LOCAL,
    SYNC_STATE_QUARANTINED,
};
use crate::sync::wire::RecordType;

#[derive(Debug, Default)]
pub struct HistorySyncStore;

impl HistorySyncStore {
    pub fn new() -> Self {
        Self
    }

    fn with_local<F, T>(f: F) -> Result<T, SyncError>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let _guard = LOCAL_MUTATION_MUTEX
            .lock()
            .map_err(|e| SyncError::Fatal(format!("history local mutation lock poisoned: {e}")))?;
        with_db(f).map_err(|e| SyncError::Fatal(format!("history db error: {e}")))
    }
}

fn db_to_local(row: &rusqlite::Row) -> rusqlite::Result<LocalRow> {
    Ok(LocalRow {
        uuid: row.get(0)?,
        timestamp_ms: row.get(1)?,
        text: row.get(2)?,
        mode: row.get(3)?,
        duration_ms: row.get(4)?,
        provider: row.get(5)?,
        model: row.get(6)?,
        language: row.get(7)?,
        deleted_at: row.get(8)?,
        server_file_id: row.get(9)?,
        sync_account: row.get(10)?,
        sync_state: row.get(11)?,
        quarantine_reason: row.get(12)?,
        rtype: RecordType::History,
        spoken: None,
        corrected: None,
        kind: None,
        trigger: None,
        expansion: None,
        settings_key: None,
        settings_value: None,
    })
}

const LOCAL_ROW_SELECT: &str = "SELECT id, timestamp_ms, text, mode, duration_ms, provider, \
     model, language, deleted_at, server_file_id, sync_account, sync_state, quarantine_reason \
     FROM history";

impl LocalStore for HistorySyncStore {
    fn list_rows(&self, account: Option<&str>) -> Vec<LocalRow> {
        let result = Self::with_local(|conn| {
            // §13 namespace: unstamped rows are visible to every account;
            // stamped rows only to their own.
            let sql = match account {
                Some(_) => {
                    format!("{LOCAL_ROW_SELECT} WHERE sync_account IS NULL OR sync_account = ?1")
                }
                None => format!("{LOCAL_ROW_SELECT} WHERE sync_account IS NULL"),
            };
            let mut stmt = conn.prepare(&sql)?;
            let iter = match account {
                Some(a) => stmt
                    .query_map(params![a], db_to_local)
                    .map_err(|e| anyhow::anyhow!(e))?,
                None => stmt
                    .query_map([], db_to_local)
                    .map_err(|e| anyhow::anyhow!(e))?,
            };
            let mut rows = Vec::new();
            for row in iter {
                rows.push(row.map_err(|e| anyhow::anyhow!(e))?);
            }
            Ok(rows)
        });
        match result {
            Ok(mut rows) => {
                rows.sort_by(|a, b| a.uuid.cmp(&b.uuid));
                rows
            }
            Err(e) => {
                log::error!("history sync store: list_rows failed: {e}");
                Vec::new()
            }
        }
    }

    fn find_row(&self, uuid: &str) -> Option<LocalRow> {
        Self::with_local(|conn| {
            let mut stmt = conn.prepare(&format!("{LOCAL_ROW_SELECT} WHERE id = ?1"))?;
            stmt.query_row(params![uuid], db_to_local)
                .optional()
                .map_err(anyhow::Error::from)
        })
        .unwrap_or(None)
    }

    fn import(&mut self, row: LocalRow) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO history
                 (id, timestamp_ms, text, mode, duration_ms, provider, char_count, timestamp,
                  model, language, deleted_at, server_file_id, sync_account, sync_state,
                  quarantine_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    row.uuid,
                    row.timestamp_ms,
                    row.text,
                    row.mode,
                    row.duration_ms,
                    row.provider,
                    row.text.chars().count() as i64,
                    chrono::DateTime::from_timestamp_millis(row.timestamp_ms)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                    row.model,
                    row.language,
                    row.deleted_at,
                    row.server_file_id,
                    row.sync_account,
                    row.sync_state,
                    row.quarantine_reason,
                ],
            )?;
            Ok(())
        })
    }

    fn mark_tombstoned(&mut self, uuid: &str, deleted_at: i64) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute(
                "UPDATE history SET deleted_at = ?1, sync_state = ?2 WHERE id = ?3",
                params![deleted_at, SYNC_STATE_DIRTY, uuid],
            )?;
            Ok(())
        })
    }

    fn set_server_file_id(&mut self, uuid: &str, file_id: &str) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute(
                "UPDATE history SET server_file_id = ?1 WHERE id = ?2",
                params![file_id, uuid],
            )?;
            Ok(())
        })
    }

    fn set_sync_state(&mut self, uuid: &str, state: &str) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute(
                "UPDATE history SET sync_state = ?1 WHERE id = ?2",
                params![state, uuid],
            )?;
            Ok(())
        })
    }

    fn quarantine(&mut self, uuid: &str, reason: QuarantineReason) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute(
                "UPDATE history SET quarantine_reason = ?1, sync_state = ?2 WHERE id = ?3",
                params![reason.as_str(), SYNC_STATE_QUARANTINED, uuid],
            )?;
            Ok(())
        })
    }

    fn clear_quarantine(&mut self, uuid: &str) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute(
                "UPDATE history SET quarantine_reason = NULL, sync_state = ?1 WHERE id = ?2",
                params![SYNC_STATE_LOCAL, uuid],
            )?;
            Ok(())
        })
    }

    fn hard_delete(&mut self, uuid: &str) -> Result<(), SyncError> {
        Self::with_local(|conn| {
            conn.execute("DELETE FROM history WHERE id = ?1", params![uuid])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::engine::SYNC_STATE_CLEAN;

    fn old_schema_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id          TEXT PRIMARY KEY,
                timestamp   TEXT NOT NULL,
                text        TEXT NOT NULL,
                mode        TEXT NOT NULL DEFAULT 'transcription',
                duration_ms INTEGER NOT NULL DEFAULT 0,
                provider    TEXT NOT NULL DEFAULT '',
                char_count  INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX idx_history_timestamp ON history(timestamp DESC);",
        )
        .unwrap();
        conn
    }

    fn insert_old_row(conn: &Connection, id: &str, timestamp: &str, text: &str) {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, mode, duration_ms, provider, char_count)
             VALUES (?1, ?2, ?3, 'transcription', 8400, 'groq', ?4)",
            params![id, timestamp, text, text.chars().count() as i64],
        )
        .unwrap();
    }

    fn migrated_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migration(&conn).unwrap();
        conn
    }

    fn insert_row(conn: &Connection, id: &str, server_file_id: Option<&str>) {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms, server_file_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                "2024-04-18T16:00:00.123Z",
                "hello",
                1713456000123i64,
                server_file_id
            ],
        )
        .unwrap();
    }

    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn old_db_gets_fifteen_columns() {
        let conn = old_schema_conn();
        insert_old_row(&conn, "id-1", "2024-04-18T16:00:00.123Z", "hello");
        run_migration(&conn).unwrap();
        let cols = column_names(&conn, "history");
        assert_eq!(cols.len(), 15);
        for name in [
            "timestamp_ms",
            "model",
            "language",
            "deleted_at",
            "sync_state",
            "server_file_id",
            "sync_account",
            "quarantine_reason",
        ] {
            assert!(cols.contains(&name.to_string()), "missing column {}", name);
        }
    }

    #[test]
    fn timestamp_ms_backfilled_from_rfc3339() {
        let conn = old_schema_conn();
        insert_old_row(&conn, "id-1", "2024-04-18T16:00:00.123Z", "hello");
        run_migration(&conn).unwrap();
        let ms: i64 = conn
            .query_row(
                "SELECT timestamp_ms FROM history WHERE id = 'id-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ms, 1713456000123);
    }

    #[test]
    fn rfc3339_parse_failure_falls_back_zero() {
        let conn = old_schema_conn();
        insert_old_row(&conn, "id-1", "not-a-date", "hello");
        run_migration(&conn).unwrap();
        let ms: i64 = conn
            .query_row(
                "SELECT timestamp_ms FROM history WHERE id = 'id-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ms, 0);
    }

    #[test]
    fn v2_backfills_zero_timestamp_ms_and_unlatches_repairable_corrupt_file_rows() {
        // Simulate the pre-v2 production shape: full schema, user_version 1,
        // zero-ts rows, and a latched corrupt-file row carrying real content.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id              TEXT PRIMARY KEY,
                timestamp       TEXT NOT NULL,
                text            TEXT NOT NULL,
                mode            TEXT NOT NULL DEFAULT 'transcription',
                duration_ms     INTEGER NOT NULL DEFAULT 0,
                provider        TEXT NOT NULL DEFAULT '',
                char_count      INTEGER NOT NULL DEFAULT 0,
                timestamp_ms    INTEGER NOT NULL DEFAULT 0,
                model           TEXT,
                language        TEXT,
                deleted_at      INTEGER,
                sync_state      TEXT NOT NULL DEFAULT 'local',
                server_file_id  TEXT,
                sync_account    TEXT,
                quarantine_reason TEXT
            );
            PRAGMA user_version = 1;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, sync_state, quarantine_reason)
             VALUES ('id-zero', '2026-08-14T12:29:01.570902600+00:00', 'meeting notes', 'transcription', 8400, 'groq', 13, 0, 'quarantined', 'corrupt_file')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, sync_state, quarantine_reason)
             VALUES ('id-empty', '2026-08-15T09:00:00.000Z', '', 'transcription', 0, 'groq', 0, 0, 'quarantined', 'corrupt_file')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, quarantine_reason)
             VALUES ('id-content-deviation', '2026-08-16T10:00:00.000Z', 'real', 'transcription', 1, 'groq', 4, 1713456000123, 'content_deviation')",
            [],
        )
        .unwrap();

        run_migration(&conn).unwrap();

        // Zero-ts backfill from the RFC3339 timestamp.
        let expected_ms =
            chrono::DateTime::parse_from_rfc3339("2026-08-14T12:29:01.570902600+00:00")
                .unwrap()
                .timestamp_millis();
        let ms: i64 = conn
            .query_row(
                "SELECT timestamp_ms FROM history WHERE id = 'id-zero'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ms, expected_ms);

        // Latched corrupt-file row with real content + valid millis is
        // unlatched so the engine's auto-repair can heal its Drive file.
        let (reason, state): (Option<String>, String) = conn
            .query_row(
                "SELECT quarantine_reason, sync_state FROM history WHERE id = 'id-zero'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(reason, None, "repairable corrupt-file row is unlatched");
        assert_eq!(state, "local");

        // Content-less placeholder stays latched.
        let (reason, state): (Option<String>, String) = conn
            .query_row(
                "SELECT quarantine_reason, sync_state FROM history WHERE id = 'id-empty'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(reason, Some("corrupt_file".to_string()));
        assert_eq!(state, "quarantined");

        // Non-corrupt-file latches are never touched.
        let reason: Option<String> = conn
            .query_row(
                "SELECT quarantine_reason FROM history WHERE id = 'id-content-deviation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, Some("content_deviation".to_string()));

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
    }

    #[test]
    fn existing_rows_unchanged() {
        let conn = old_schema_conn();
        insert_old_row(&conn, "id-1", "2024-04-18T16:00:00.123Z", "first");
        insert_old_row(&conn, "id-2", "2024-04-19T16:00:00.123Z", "second");
        run_migration(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        let (id, ts, text, mode, dur, prov, chars): (
            String,
            String,
            String,
            String,
            i64,
            String,
            i64,
        ) = conn
            .query_row(
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count
                 FROM history WHERE id = 'id-2'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(id, "id-2");
        assert_eq!(ts, "2024-04-19T16:00:00.123Z");
        assert_eq!(text, "second");
        assert_eq!(mode, "transcription");
        assert_eq!(dur, 8400);
        assert_eq!(prov, "groq");
        assert_eq!(chars, 6);
    }

    #[test]
    fn user_version_set_to_2() {
        let conn = old_schema_conn();
        run_migration(&conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);

        // Fresh (empty) DB also lands on version 2 with the full schema.
        let fresh = Connection::open_in_memory().unwrap();
        run_migration(&fresh).unwrap();
        let v2: i64 = fresh
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v2, 2);
        assert_eq!(column_names(&fresh, "history").len(), 15);

        // Idempotent: a second run leaves the schema and version untouched.
        run_migration(&fresh).unwrap();
        let v3: i64 = fresh
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v3, 2);
        assert_eq!(column_names(&fresh, "history").len(), 15);
    }

    #[test]
    fn failed_migration_rolls_back_and_disables_sync() {
        MIGRATION_OK.store(true, Ordering::SeqCst);
        let conn = old_schema_conn();
        insert_old_row(&conn, "id-1", "2024-04-18T16:00:00.123Z", "hello");
        // Force the ALTER for `deleted_at` to fail (duplicate column name).
        conn.execute_batch("ALTER TABLE history ADD COLUMN deleted_at INTEGER")
            .unwrap();

        let res = run_migration(&conn);
        assert!(res.is_ok(), "migration failure must not break init_db");
        assert!(!MIGRATION_OK.load(Ordering::SeqCst));

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0, "user_version must stay 0 after rollback");

        let cols = column_names(&conn, "history");
        for name in [
            "timestamp_ms",
            "model",
            "language",
            "sync_state",
            "server_file_id",
            "sync_account",
            "quarantine_reason",
        ] {
            assert!(
                !cols.contains(&name.to_string()),
                "partial column {} must be rolled back",
                name
            );
        }

        // Row intact; reads still work against the old schema.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let text: String = conn
            .query_row("SELECT text FROM history WHERE id = 'id-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn never_uploaded_delete_hard_deletes() {
        let conn = migrated_conn();
        insert_row(&conn, "id-1", None);
        delete_history_row(&conn, "id-1", None).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn uploaded_delete_tombstones() {
        let conn = migrated_conn();
        insert_row(&conn, "id-1", Some("file-1"));
        delete_history_row(&conn, "id-1", None).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "uploaded row must remain as a tombstone");

        let (deleted_at, sync_state): (Option<i64>, String) = conn
            .query_row(
                "SELECT deleted_at, sync_state FROM history WHERE id = 'id-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(deleted_at.is_some(), "deleted_at must be set");
        assert_eq!(sync_state, "dirty");

        let text: String = conn
            .query_row("SELECT text FROM history WHERE id = 'id-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn foreign_account_delete_is_rejected() {
        let conn = migrated_conn();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms, server_file_id, sync_account)
             VALUES ('id-f', '2024-04-18T16:00:00.123Z', 'hello', 1713456000123, 'file-f', 'other@example.com')",
            [],
        )
        .unwrap();

        let res = delete_history_row(&conn, "id-f", Some("me@example.com"));
        assert!(res.is_err(), "foreign row must be read-only");
        let res = delete_history_row(&conn, "id-f", None);
        assert!(
            res.is_err(),
            "unstamped context must not touch foreign rows"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "foreign row must be untouched");
    }

    #[test]
    fn own_account_delete_is_allowed() {
        let conn = migrated_conn();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms, server_file_id, sync_account)
             VALUES ('id-own', '2024-04-18T16:00:00.123Z', 'hello', 1713456000123, 'file-own', 'me@example.com')",
            [],
        )
        .unwrap();

        delete_history_row(&conn, "id-own", Some("me@example.com")).unwrap();
        let (deleted_at, sync_state): (Option<i64>, String) = conn
            .query_row(
                "SELECT deleted_at, sync_state FROM history WHERE id = 'id-own'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(deleted_at.is_some());
        assert_eq!(sync_state, "dirty");
    }

    #[test]
    fn already_tombstoned_delete_is_noop() {
        let conn = migrated_conn();
        insert_row(&conn, "id-1", Some("file-1"));
        conn.execute(
            "UPDATE history SET deleted_at = 123, sync_state = 'dirty' WHERE id = 'id-1'",
            [],
        )
        .unwrap();

        delete_history_row(&conn, "id-1", None).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let (deleted_at, sync_state): (Option<i64>, String) = conn
            .query_row(
                "SELECT deleted_at, sync_state FROM history WHERE id = 'id-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(deleted_at, Some(123), "tombstone must not change");
        assert_eq!(sync_state, "dirty");
    }

    #[test]
    fn clear_history_splits_unsynced_and_tombstones_synced() {
        let conn = migrated_conn();
        insert_row(&conn, "id-1", None);
        insert_row(&conn, "id-2", Some("file-2"));

        clear_history_rows(&conn, None).unwrap();

        let remaining: Vec<(String, Option<i64>, String)> = conn
            .prepare("SELECT id, deleted_at, sync_state FROM history")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "never-uploaded row must be hard-deleted"
        );
        assert_eq!(remaining[0].0, "id-2");
        assert!(remaining[0].1.is_some(), "uploaded row must be tombstoned");
        assert_eq!(remaining[0].2, "dirty");
    }

    #[test]
    fn weekly_activity_boundary_uses_timestamp_ms() {
        let conn = migrated_conn();
        insert_row(&conn, "id-in-week", None); // 16:00:00.123Z, ms 1713456000123
                                               // Sub-millisecond fraction that sorts BEFORE ".000Z" lexicographically
                                               // but AFTER the boundary instant chronologically: the old string
                                               // comparison wrongly excluded it.
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                "id-fraction",
                "2024-04-18T16:00:00.0004+00:00",
                "hello",
                1713456000004i64
            ],
        )
        .unwrap();
        // Just before the boundary instant: must stay excluded.
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                "id-before",
                "2024-04-18T15:59:59.999Z",
                "hello",
                1713455999999i64
            ],
        )
        .unwrap();
        // After the boundary but deleted: must stay excluded.
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                "id-deleted",
                "2024-04-18T16:00:00.200Z",
                "hello",
                1713456000200i64,
                1713456000200i64
            ],
        )
        .unwrap();

        let boundary_ms = chrono::DateTime::parse_from_rfc3339("2024-04-18T16:00:00.000Z")
            .unwrap()
            .timestamp_millis();
        let timestamps = weekly_activity(&conn, boundary_ms).unwrap();

        assert!(timestamps.contains(&"2024-04-18T16:00:00.123Z".to_string()));
        assert!(timestamps.contains(&"2024-04-18T16:00:00.0004+00:00".to_string()));
        assert!(!timestamps.contains(&"2024-04-18T15:59:59.999Z".to_string()));
        assert!(!timestamps.contains(&"2024-04-18T16:00:00.200Z".to_string()));
    }

    fn ms(ts: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn utc_week_start_is_monday_midnight() {
        // 2024-04-18 is a Thursday; week starts Monday 2024-04-15T00:00:00Z.
        assert_eq!(
            utc_week_start_ms(ms("2024-04-18T16:00:00.123Z")),
            ms("2024-04-15T00:00:00.000Z")
        );
        // Sunday 2024-04-14 still belongs to the week starting Monday 04-08.
        assert_eq!(
            utc_week_start_ms(ms("2024-04-14T23:59:59.999Z")),
            ms("2024-04-08T00:00:00.000Z")
        );
        // Monday midnight is its own week start.
        assert_eq!(
            utc_week_start_ms(ms("2024-04-15T00:00:00.000Z")),
            ms("2024-04-15T00:00:00.000Z")
        );
    }

    #[test]
    fn utc_month_start_is_first_midnight() {
        assert_eq!(
            utc_month_start_ms(ms("2024-04-18T16:00:00.123Z")),
            ms("2024-04-01T00:00:00.000Z")
        );
        assert_eq!(
            utc_month_start_ms(ms("2024-04-01T00:00:00.000Z")),
            ms("2024-04-01T00:00:00.000Z")
        );
        assert_eq!(
            utc_month_start_ms(ms("2024-12-31T23:59:59.999Z")),
            ms("2024-12-01T00:00:00.000Z")
        );
    }

    #[test]
    fn word_count_splits_on_whitespace() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("hello"), 1);
        assert_eq!(word_count("  hello   world  "), 2);
        assert_eq!(word_count("hello\nworld\tagain"), 3);
    }

    #[test]
    fn compute_stats_buckets_week_and_month() {
        let now = ms("2024-04-18T16:00:00.000Z");
        let rows = vec![
            (
                ms("2024-04-18T10:00:00.000Z"),
                10_000i64,
                "hello world".to_string(),
            ), // this week, this month
            (ms("2024-04-10T10:00:00.000Z"), 5_000i64, "foo".to_string()), // this month, last week
            (
                ms("2024-03-25T10:00:00.000Z"),
                2_000i64,
                "bar baz qux".to_string(),
            ), // last month
        ];
        let stats = compute_stats(&rows, now);

        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.total_chars, 11 + 3 + 11);
        assert_eq!(stats.total_duration_ms, 17_000);
        assert_eq!(stats.total_words, 6);
        assert_eq!(stats.weekly_count, 1);
        assert_eq!(stats.weekly_duration_ms, 10_000);
        assert_eq!(stats.weekly_words, 2);
        assert_eq!(stats.monthly_count, 2);
        assert_eq!(stats.monthly_words, 3);
        assert_eq!(stats.week_start_ms, ms("2024-04-15T00:00:00.000Z"));
        assert_eq!(stats.month_start_ms, ms("2024-04-01T00:00:00.000Z"));
    }

    fn insert_stamped(conn: &Connection, id: &str, account: Option<&str>) {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms, sync_account, sync_state)
             VALUES (?1, '2024-04-18T16:00:00.123Z', 'hello', 1713456000123, ?2, 'local')",
            params![id, account],
        )
        .unwrap();
    }

    #[test]
    fn clear_history_skips_foreign_rows() {
        let conn = migrated_conn();
        insert_row(&conn, "id-local", None);
        insert_stamped(&conn, "id-own", Some("me@example.com"));
        conn.execute(
            "UPDATE history SET server_file_id = 'file-own' WHERE id = 'id-own'",
            [],
        )
        .unwrap();
        insert_stamped(&conn, "id-foreign", Some("other@example.com"));
        conn.execute(
            "UPDATE history SET server_file_id = 'file-f' WHERE id = 'id-foreign'",
            [],
        )
        .unwrap();
        insert_stamped(&conn, "id-foreign-nosfi", Some("other@example.com"));

        clear_history_rows(&conn, Some("me@example.com")).unwrap();

        let remaining: Vec<(String, Option<i64>)> = conn
            .prepare("SELECT id, deleted_at FROM history")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(remaining.len(), 3, "local row hard-deleted, foreign kept");
        let own = remaining
            .iter()
            .find(|(id, _)| id == "id-own")
            .expect("own row kept as tombstone");
        assert!(own.1.is_some(), "own account row tombstoned");
        for (id, deleted_at) in &remaining {
            if id != "id-own" {
                assert!(deleted_at.is_none(), "foreign row {id} must be untouched");
            }
        }
    }

    // HistorySyncStore exercises run against the static DB (set to an
    // in-memory migrated connection). They are intentionally sequential in
    // one test: the static DB is process-global and parallel tests would race
    // on it.
    #[test]
    fn history_sync_store_roundtrip_and_ops() {
        let conn = Connection::open_in_memory().unwrap();
        run_migration(&conn).unwrap();
        *DB.lock().unwrap() = Some(conn);

        let mut store = HistorySyncStore::new();
        let row = LocalRow {
            uuid: "row-1".to_string(),
            timestamp_ms: 1713456000123,
            text: "hello world".to_string(),
            mode: "transcription".to_string(),
            duration_ms: 8400,
            provider: "groq".to_string(),
            model: None,
            language: Some("en".to_string()),
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
            sync_account: Some("me@example.com".to_string()),
            sync_state: SYNC_STATE_CLEAN.to_string(),
            quarantine_reason: None,
        };
        store.import(row.clone()).unwrap();

        // §13 account namespace: the stamped row is visible to its own
        // account and invisible to every other context.
        assert_eq!(store.list_rows(Some("me@example.com")).len(), 1);
        assert!(store.list_rows(Some("other@example.com")).is_empty());
        assert!(store.list_rows(None).is_empty());

        let found = store.find_row("row-1").expect("imported row found");
        assert_eq!(found.uuid, row.uuid);
        assert_eq!(found.timestamp_ms, row.timestamp_ms);
        assert_eq!(found.text, row.text);
        assert_eq!(found.duration_ms, row.duration_ms);
        assert_eq!(found.provider, row.provider);
        assert_eq!(found.sync_account, row.sync_account);
        assert_eq!(found.sync_state, SYNC_STATE_CLEAN);
        assert_eq!(
            found.to_wire().to_json(),
            row.to_wire().to_json(),
            "wire JSON must survive the DB roundtrip byte-identical"
        );

        store.mark_tombstoned("row-1", 1713456000999).unwrap();
        let t = store.find_row("row-1").unwrap();
        assert_eq!(t.deleted_at, Some(1713456000999));
        assert_eq!(t.sync_state, SYNC_STATE_DIRTY);

        store.set_server_file_id("row-1", "file-1").unwrap();
        store.set_sync_state("row-1", SYNC_STATE_CLEAN).unwrap();
        let s = store.find_row("row-1").unwrap();
        assert_eq!(s.server_file_id.as_deref(), Some("file-1"));
        assert_eq!(s.sync_state, SYNC_STATE_CLEAN);

        store
            .quarantine("row-1", QuarantineReason::ContentDeviation)
            .unwrap();
        let q = store.find_row("row-1").unwrap();
        assert_eq!(q.quarantine_reason.as_deref(), Some("content_deviation"));
        assert_eq!(q.sync_state, SYNC_STATE_QUARANTINED);
        assert!(q.is_latched());

        store.clear_quarantine("row-1").unwrap();
        let c = store.find_row("row-1").unwrap();
        assert!(c.quarantine_reason.is_none());
        assert_eq!(c.sync_state, SYNC_STATE_LOCAL);

        // An unstamped imported row is visible in the null namespace.
        let mut unowned = row.clone();
        unowned.uuid = "row-2".to_string();
        unowned.sync_account = None;
        store.import(unowned).unwrap();
        assert_eq!(store.list_rows(None).len(), 1);

        store.hard_delete("row-1").unwrap();
        assert!(store.find_row("row-1").is_none());
        assert_eq!(store.list_rows(None).len(), 1);
    }
}
