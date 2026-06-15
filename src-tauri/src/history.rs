// Fluence Windows — Transcription History module
// SQLite database in app data directory for storing past transcription sessions.

use anyhow::Result;
use chrono::Utc;
use dirs::data_local_dir;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Emitter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: String,
    pub text: String,
    pub mode: String,         // "transcription" | "agent"
    pub duration_ms: u64,
    pub provider: String,
    pub char_count: usize,
}

static DB: Mutex<Option<Connection>> = Mutex::new(None);

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
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS history (
            id          TEXT PRIMARY KEY,
            timestamp   TEXT NOT NULL,
            text        TEXT NOT NULL,
            mode        TEXT NOT NULL DEFAULT 'transcription',
            duration_ms INTEGER NOT NULL DEFAULT 0,
            provider    TEXT NOT NULL DEFAULT '',
            char_count  INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(timestamp DESC);",
    )?;

    let mut db = DB.lock().map_err(|e| anyhow::anyhow!("DB lock poisoned: {}", e))?;
    *db = Some(conn);
    Ok(())
}

fn with_db<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T>,
{
    // Try to get connection; if None, initialize first (outside the lock scope)
    let needs_init = {
        let db = DB.lock().map_err(|e| anyhow::anyhow!("DB lock poisoned: {}", e))?;
        db.is_none()
    };
    if needs_init {
        init_db()?;
    }
    let db = DB.lock().map_err(|e| anyhow::anyhow!("DB lock poisoned: {}", e))?;
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
    let entry = HistoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        text: text.to_string(),
        mode: mode.to_string(),
        duration_ms,
        provider: provider.to_string(),
        char_count: text.chars().count(),
    };

    with_db(|conn| {
        conn.execute(
            "INSERT INTO history (id, timestamp, text, mode, duration_ms, provider, char_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.id,
                entry.timestamp,
                entry.text,
                entry.mode,
                entry.duration_ms as i64,
                entry.provider,
                entry.char_count as i64,
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
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count
                 FROM history ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
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
                "SELECT id, timestamp, text, mode, duration_ms, provider, char_count
                 FROM history WHERE text LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC LIMIT ?2 OFFSET ?3",
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
    let entry = add_history_entry(&text, &mode, duration_ms, &provider).map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(entry)
}

#[tauri::command]
pub fn delete_history_entry(app: tauri::AppHandle, id: String) -> Result<(), String> {
    with_db(|conn| {
        conn.execute("DELETE FROM history WHERE id = ?1", params![id])?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(())
}

#[tauri::command]
pub fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    with_db(|conn| {
        conn.execute("DELETE FROM history", [])?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    let _ = app.emit("history-updated", ());
    Ok(())
}

#[tauri::command]
pub fn get_history_stats() -> Result<serde_json::Value, String> {
    with_db(|conn| {
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
            .unwrap_or(0);
        let total_chars: i64 = conn
            .query_row("SELECT COALESCE(SUM(char_count),0) FROM history", [], |r| r.get(0))
            .unwrap_or(0);
        let total_duration_ms: i64 = conn
            .query_row("SELECT COALESCE(SUM(duration_ms),0) FROM history", [], |r| r.get(0))
            .unwrap_or(0);
        Ok(serde_json::json!({
            "total_entries": total,
            "total_chars": total_chars,
            "total_duration_ms": total_duration_ms
        }))
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_weekly_activity(start_of_week_utc: String) -> Result<Vec<String>, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare("SELECT timestamp FROM history WHERE timestamp >= ?1")?;
        let rows = stmt.query_map(params![start_of_week_utc], |r| r.get::<_, String>(0))?;
        let mut timestamps = Vec::new();
        for ts in rows.flatten() {
            timestamps.push(ts);
        }
        Ok(timestamps)
    })
    .map_err(|e| e.to_string())
}
