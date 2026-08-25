// Fluence Windows — Transcription History module
// SQLite database in app data directory for storing past transcription sessions.

use anyhow::Result;
use chrono::{Datelike, TimeZone, Utc};
use dirs::data_local_dir;
use rusqlite::{params, Connection};
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
    // Frozen v1.1: history stays local, no sync columns. Initial create is frozen schema.
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
            deleted_at      INTEGER
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
    // v2 → v3 (frozen v1.1): history stays local, drop sync columns (recreate)
    let current_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current_version < 3 {
        if let Err(e) = migrate_v2_to_v3(conn) {
            log::error!("History DB v3 migration failed: {}", e);
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
            .map_or(0, |dt| dt.timestamp_millis());
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
            .map_or(0, |dt| dt.timestamp_millis());
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

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    // Check if sync columns exist
    let has_sync = {
        let mut stmt = conn.prepare("PRAGMA table_info(history)")?;
        let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
        cols.contains(&"sync_state".to_string())
    };
    if !has_sync {
        conn.execute_batch("PRAGMA user_version = 3")?;
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    // Recreate without sync columns, keep core history data
    tx.execute_batch(
        "CREATE TABLE history_new (
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
            deleted_at      INTEGER
        );
        INSERT INTO history_new (id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, model, language, deleted_at)
            SELECT id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, model, language, deleted_at FROM history;
        DROP TABLE history;
        ALTER TABLE history_new RENAME TO history;
        CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(timestamp DESC);
        PRAGMA user_version = 3",
    )?;
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
    let _io = crate::sync::io_lock::io_lock_guard();
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
        sync_state: None,
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

    // Account-level statistics: every completed dictation contributes exactly
    // one stats event (deterministic id per history row — duplicates collapse
    // under union dedup). Safe offline; the event rides the next sync.
    // Transcription history itself NEVER leaves this device.
    crate::sync::stores::StatsDirtyStore::record_dictation_event(
        &entry.id,
        now.timestamp_millis(),
        &entry.text,
        entry.duration_ms as i64,
    );

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
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, model, language, deleted_at
                 FROM history WHERE deleted_at IS NULL
                 ORDER BY timestamp_ms DESC LIMIT ?1 OFFSET ?2",
            )?;
            let res = stmt.query_map(params![page_size, offset], map_row)
                .map_err(|e| anyhow::anyhow!(e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow::anyhow!(e));
            res
        } else {
            let escaped_query = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let pattern = format!("%{}%", escaped_query);
            let mut stmt = conn.prepare(
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count, timestamp_ms, model, language, deleted_at
                 FROM history WHERE text LIKE ?1 ESCAPE '\\' AND deleted_at IS NULL
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
    let deleted: Option<i64> = row.get(10)?;
    if deleted.is_some() {
        // Deleted rows are filtered at query, but handle Tombstone still
    }
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
        sync_account: None,
        quarantine_reason: None,
        sync_state: None,
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

// Frozen v1.1: history stays local — no sync, hard delete always.
pub(crate) fn delete_history_by_id(id: &str) -> Result<()> {
    let _guard = LOCAL_MUTATION_MUTEX
        .lock()
        .map_err(|e| anyhow::anyhow!("local mutation lock poisoned: {}", e))?;
    with_db(|conn| {
        conn.execute("DELETE FROM history WHERE id = ?1", params![id])?;
        Ok(())
    })
}

#[tauri::command]
pub fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    clear_all_history().map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(())
}

pub(crate) fn clear_all_history() -> Result<()> {
    let _guard = LOCAL_MUTATION_MUTEX
        .lock()
        .map_err(|e| anyhow::anyhow!("local mutation lock poisoned: {}", e))?;
    with_db(|conn| clear_history_rows(conn, None))
}

fn clear_history_rows(conn: &Connection, _active_account: Option<&str>) -> Result<()> {
    conn.execute("DELETE FROM history WHERE 1=1", [])?;
    Ok(())
}

// UTC midnight on the Monday of the week containing `now_ms`. All statistics
// bucketing uses this boundary so every device computes identical buckets.
pub(crate) fn utc_week_start_ms(now_ms: i64) -> i64 {
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
pub(crate) fn utc_month_start_ms(now_ms: i64) -> i64 {
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
            "SELECT timestamp_ms, duration_ms, text FROM history WHERE deleted_at IS NULL",
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

fn weekly_activity(conn: &Connection, week_start_ms: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT timestamp FROM history WHERE timestamp_ms >= ?1 AND deleted_at IS NULL",
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn insert_row(conn: &Connection, id: &str, _server_file_id: Option<&str>) {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                "2024-04-18T16:00:00.123Z",
                "hello",
                1713456000123i64
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
        // Frozen v1.1: history stays local, sync columns dropped -> 11 columns
        assert!(cols.contains(&"timestamp_ms".to_string()));
        assert!(cols.contains(&"model".to_string()));
        assert!(cols.contains(&"language".to_string()));
        // sync columns should be absent
        for name in ["sync_state", "server_file_id", "sync_account", "quarantine_reason"] {
            assert!(!cols.contains(&name.to_string()), "obsolete column {} should be dropped", name);
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
        // Frozen: old sync columns are dropped; this test now just checks migration drops them and version >=3
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
        run_migration(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert!(v >= 3);
        // After frozen migration, sync columns should be gone
        let cols = column_names(&conn, "history");
        assert!(!cols.contains(&"sync_state".to_string()));
        // timestamp_ms should be backfilled or at least not missing
        let ms: i64 = conn.query_row("SELECT timestamp_ms FROM history WHERE id = 'id-zero'", [], |r| r.get(0)).unwrap();
        assert!(ms > 0);
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
        assert!(v >= 3, "frozen should be >=3");

        let fresh = Connection::open_in_memory().unwrap();
        run_migration(&fresh).unwrap();
        let v2: i64 = fresh
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert!(v2 >= 3);
        // Idempotent
        run_migration(&fresh).unwrap();
        let v3: i64 = fresh
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v3, v2);
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
        conn.execute("DELETE FROM history WHERE id = 'id-1'", []).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn uploaded_delete_tombstones() {
        // Frozen v1.2: history stays local, delete is always hard delete
        let conn = migrated_conn();
        insert_row(&conn, "id-1", Some("file-1"));
        conn.execute("DELETE FROM history WHERE id = 'id-1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "frozen: history hard deletes always");
    }

    #[test]
    fn foreign_account_delete_is_rejected() {
        // Frozen: no account isolation for history (local only), always deletable
        let conn = migrated_conn();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms)
             VALUES ('id-f', '2024-04-18T16:00:00.123Z', 'hello', 1713456000123)",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM history WHERE id = 'id-f'", []).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn own_account_delete_is_allowed() {
        let conn = migrated_conn();
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms)
             VALUES ('id-own', '2024-04-18T16:00:00.123Z', 'hello', 1713456000123)",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM history WHERE id = 'id-own'", []).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn already_tombstoned_delete_is_noop() {
        let conn = migrated_conn();
        insert_row(&conn, "id-1", Some("file-1"));
        conn.execute(
            "UPDATE history SET deleted_at = 123 WHERE id = 'id-1'",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM history WHERE id = 'id-1'", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "frozen: delete always hard deletes");
    }

    #[test]
    fn clear_history_splits_unsynced_and_tombstones_synced() {
        let conn = migrated_conn();
        insert_row(&conn, "id-1", None);
        insert_row(&conn, "id-2", Some("file-2"));

        clear_history_rows(&conn, None).unwrap();

        let count: i64 = conn.prepare("SELECT COUNT(*) FROM history").unwrap().query_row([], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "frozen: clear deletes all");
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

    fn insert_stamped(conn: &Connection, id: &str, _account: Option<&str>) {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, timestamp_ms)
             VALUES (?1, '2024-04-18T16:00:00.123Z', 'hello', 1713456000123)",
            params![id],
        )
        .unwrap();
    }

    #[test]
    fn clear_history_skips_foreign_rows() {
        let conn = migrated_conn();
        insert_row(&conn, "id-local", None);
        insert_stamped(&conn, "id-own", Some("me@example.com"));
        insert_stamped(&conn, "id-foreign", Some("other@example.com"));
        insert_stamped(&conn, "id-foreign-nosfi", Some("other@example.com"));

        clear_history_rows(&conn, Some("me@example.com")).unwrap();

        let count: i64 = conn.prepare("SELECT COUNT(*) FROM history").unwrap().query_row([], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "frozen: clear deletes all history");
    }

    #[test]
    fn dictation_commit_records_exactly_one_stats_event() {
        // Every completed dictation must contribute exactly one account-level
        // stats event; a duplicated commit path collapses under union dedup.
        let tmp = std::env::temp_dir().join(format!("fluence-test-ledger-hist-{}.json", std::process::id()));
        crate::sync::stores::StatsDirtyStore::set_test_ledger_path(Some(tmp.clone()));
        let conn = Connection::open_in_memory().unwrap();
        run_migration(&conn).unwrap();
        *DB.lock().unwrap() = Some(conn);

        let entry = add_history_entry("hello world", "transcription", 1000, "groq").unwrap();
        add_history_entry("second entry", "transcription", 500, "groq").unwrap();

        let rows = crate::sync::stores::StatsDirtyStore::test_rows();
        let ids: Vec<&str> = rows.iter().map(|r| r.item.event_id.as_str()).collect();
        assert_eq!(ids.len(), 2, "one event per dictation");
        assert!(ids.contains(&crate::sync::domain::synthetic_event_id(&entry.id).as_str()));

        // Re-committing the same row id must not create another event.
        crate::sync::stores::StatsDirtyStore::record_dictation_event(
            &entry.id,
            1_000,
            "hello world",
            1000,
        );
        let rows = crate::sync::stores::StatsDirtyStore::test_rows();
        assert_eq!(rows.len(), 2, "exactly-once by deterministic id");
        crate::sync::stores::StatsDirtyStore::set_test_ledger_path(None);
        let _ = std::fs::remove_file(&tmp);
    }
}
