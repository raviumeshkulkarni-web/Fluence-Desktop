// Fluence sync — frozen v1.2 domain envelopes (dictionary, snippets, stats, settings)
//
// Drive: drive.appdata, appDataFolder/fluence/v1/{dictionary.json,snippets.json,stats.json,settings.json}
//
// Validation policy (v1.2 hardening):
// - Envelope version must be exactly 1; anything else is skipped as foreign.
// - Individual records that fail validation are SKIPPED, never applied —
//   one malformed record must not discard an otherwise-valid domain.
// - Envelopes exceeding the item cap or byte cap are rejected wholesale
//   (corruption/abuse guard).
// - businessKey identity is always recomputed from record CONTENT
//   (`business_key()` derives from spoken/trigger) — never trusted from wire.
// - Duplicate domain files on Drive are all fetched and merged by the engine.

use serde::{Deserialize, Serialize};

pub const ENVELOPE_V1: i32 = 1;

/// Maximum records accepted in one envelope. Legitimate accounts hold tens to
/// hundreds of dictionary words / snippets and thousands of stat events;
/// anything beyond this bound is corruption or abuse.
pub const MAX_ENVELOPE_ITEMS: usize = 10_000;

// ── Dictionary ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DictionaryItem {
    #[serde(rename = "syncId")]
    pub sync_id: String,
    pub spoken: String,
    pub corrected: String,
    pub kind: String, // correction | expansion
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
    #[serde(rename = "deletedAt")]
    pub deleted_at: Option<i64>,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

impl DictionaryItem {
    /// Canonical identity — ALWAYS derived from content, never from any wire
    /// field. Case-insensitive on the spoken form.
    pub fn business_key(&self) -> String {
        self.spoken.trim().to_lowercase()
    }

    /// Compact fingerprint of the user-visible state, used for dirty checks.
    pub fn canonical_dirty(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.spoken.trim().to_lowercase(),
            self.corrected,
            self.kind,
            self.is_enabled
        )
    }

    /// Per-record validation. Invalid records are skipped at ingest.
    pub fn validate(&self) -> bool {
        if uuid::Uuid::parse_str(&self.sync_id).is_err() {
            return false;
        }
        if self.device_id.is_empty() {
            return false;
        }
        if self.updated_at <= 0 {
            return false;
        }
        if let Some(d) = self.deleted_at {
            if d <= 0 {
                return false;
            }
        }
        if self.kind != "correction" && self.kind != "expansion" {
            return false;
        }
        if self.spoken.trim().is_empty() || self.corrected.trim().is_empty() {
            return false;
        }
        // Bound string sizes: a hostile record cannot balloon memory.
        self.spoken.len() <= 4096 && self.corrected.len() <= 4096
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DictionaryEnvelope {
    pub v: i32,
    #[serde(default)]
    pub items: Vec<DictionaryItem>,
}

impl DictionaryEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Lenient ingest: unknown fields ignored, invalid ITEMS skipped, only a
    /// wrong version or oversized item count poisons the whole envelope.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.items.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            items: env.items.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

// ── Snippets ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnippetItem {
    #[serde(rename = "syncId")]
    pub sync_id: String,
    pub trigger: String,
    pub expansion: String,
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
    #[serde(rename = "deletedAt")]
    pub deleted_at: Option<i64>,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

impl SnippetItem {
    /// Canonical identity — derived from the trigger content.
    pub fn business_key(&self) -> String {
        self.trigger.trim().to_lowercase()
    }

    pub fn canonical_dirty(&self) -> String {
        format!("{}|{}", self.trigger.trim().to_lowercase(), self.expansion)
    }

    pub fn validate(&self) -> bool {
        if uuid::Uuid::parse_str(&self.sync_id).is_err() {
            return false;
        }
        if self.device_id.is_empty() || self.updated_at <= 0 {
            return false;
        }
        if let Some(d) = self.deleted_at {
            if d <= 0 {
                return false;
            }
        }
        if self.trigger.trim().is_empty() || self.expansion.is_empty() {
            return false;
        }
        self.trigger.len() <= 4096 && self.expansion.len() <= 8192
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnippetEnvelope {
    pub v: i32,
    #[serde(default)]
    pub items: Vec<SnippetItem>,
}

impl SnippetEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.items.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            items: env.items.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

// ── Settings ────────────────────────────────────────────────────────────────
// Per-key LWW for the frozen five keys only. Platform-specific configuration
// (hotkeys, providers, API keys, audio devices, UI) can never enter this
// domain because non-whitelisted keys are rejected at ingest AND at emit.

pub const SETTINGS_KEYS: [&str; 5] = [
    "language",
    "dictionary_enabled",
    "snippets_enabled",
    "auto_learn_enabled",
    "ai_polish_style",
];

pub fn is_allowed_settings_key(k: &str) -> bool {
    SETTINGS_KEYS.contains(&k)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsItem {
    pub key: String,
    pub value: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

impl SettingsItem {
    pub fn validate(&self) -> bool {
        if !is_allowed_settings_key(&self.key) {
            return false;
        }
        if self.updated_at <= 0 || self.device_id.is_empty() {
            return false;
        }
        self.value.len() <= 1024
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsEnvelope {
    pub v: i32,
    #[serde(default)]
    pub items: Vec<SettingsItem>,
}

impl SettingsEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.items.len() > SETTINGS_KEYS.len() * 4 {
            return None;
        }
        Some(Self {
            v: env.v,
            items: env.items.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

// ── Stats ───────────────────────────────────────────────────────────────────
// Event-sourced union dedup by eventId. One event per completed dictation;
// totals are summed at display time so no event can ever be counted twice.
// Backfill of pre-sync history uses deterministic UUIDv5 ids derived from the
// history row id, making re-runs idempotent under union dedup.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StatsItem {
    #[serde(rename = "eventId")]
    pub event_id: String,
    pub day: String, // YYYY-MM-DD UTC
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: i64,
    #[serde(default)]
    pub words: Option<i64>,
    #[serde(default)]
    pub chars: Option<i64>,
    #[serde(default)]
    pub durationMs: Option<i64>,
}

impl StatsItem {
    pub fn validate(&self) -> bool {
        if uuid::Uuid::parse_str(&self.event_id).is_err() {
            return false;
        }
        if self.day.len() != 10 {
            return false;
        }
        if chrono::NaiveDate::parse_from_str(&self.day, "%Y-%m-%d").is_err() {
            return false;
        }
        if self.timestamp_ms <= 0 {
            return false;
        }
        // Sanity: a single dictation cannot contribute absurd magnitudes.
        let words = self.words.unwrap_or(0);
        let chars = self.chars.unwrap_or(0);
        let dur = self.durationMs.unwrap_or(0);
        (0..=1_000_000).contains(&words)
            && (0..=10_000_000).contains(&chars)
            && (0..=86_400_000 * 7).contains(&dur)
    }

    /// Fresh event for one completed dictation. `event_id` is deterministic
    /// per history row (UUIDv5 of the row id) so even a duplicated commit
    /// path produces the SAME event id and union dedup absorbs it — exactly
    /// once counting by construction.
    pub fn from_history_row(history_id: &str, timestamp_ms: i64, text: &str, duration_ms: i64) -> Self {
        let day = chrono::DateTime::from_timestamp_millis(timestamp_ms)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "1970-01-01".to_string());
        Self {
            event_id: synthetic_event_id(history_id),
            day,
            timestamp_ms,
            words: Some(text.split_whitespace().count() as i64),
            chars: Some(text.chars().count() as i64),
            durationMs: Some(duration_ms),
        }
    }
}

/// Deterministic UUIDv5 event id for a history row id. Stable across
/// platforms, accounts and retries.
pub fn synthetic_event_id(history_id: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, history_id.as_bytes()).to_string()
}

/// Deterministic UUIDv5 backfill id for legacy imports (kept for parity with
/// Android's backfill naming).
pub fn synthetic_backfill_id(history_id: &str, account_hash: &str) -> String {
    let name = format!("{history_id}:{account_hash}");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsEnvelope {
    pub v: i32,
    #[serde(default)]
    pub items: Vec<StatsItem>,
}

impl StatsEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.items.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            items: env.items.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(spoken: &str, corrected: &str, updated_at: i64) -> DictionaryItem {
        DictionaryItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            spoken: spoken.to_string(),
            corrected: corrected.to_string(),
            kind: "correction".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at,
            device_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    #[test]
    fn business_key_is_always_content_derived() {
        let item = dict("  Hello ", "hi", 100);
        assert_eq!(item.business_key(), "hello");
    }

    #[test]
    fn invalid_items_are_skipped_not_fatal() {
        let good = dict("hello", "hi", 100);
        let mut bad = dict("world", "mundo", 200);
        bad.sync_id = "not-a-uuid".to_string();
        let env = DictionaryEnvelope { v: 1, items: vec![good, bad] };
        let bytes = env.to_bytes();
        let parsed = DictionaryEnvelope::from_bytes(&bytes).expect("envelope parses");
        assert_eq!(parsed.items.len(), 1, "invalid item skipped, valid kept");
        assert_eq!(parsed.items[0].spoken, "hello");
    }

    #[test]
    fn wrong_version_rejects_whole_envelope() {
        let env = DictionaryEnvelope { v: 2, items: vec![dict("a", "b", 1)] };
        assert!(DictionaryEnvelope::from_bytes(&env.to_bytes()).is_none());
    }

    #[test]
    fn oversized_item_count_rejects_envelope() {
        let items: Vec<DictionaryItem> = (0..MAX_ENVELOPE_ITEMS + 1)
            .map(|i| dict(&format!("w{i}"), "x", 1))
            .collect();
        let env = DictionaryEnvelope { v: 1, items };
        assert!(DictionaryEnvelope::from_bytes(&env.to_bytes()).is_none());
    }

    #[test]
    fn stats_validation_bounds_magnitudes() {
        let ok = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-21".to_string(),
            timestamp_ms: 1_000,
            words: Some(500),
            chars: Some(2000),
            durationMs: Some(60_000),
        };
        assert!(ok.validate());
        let absurd = StatsItem { words: Some(99_000_000), ..ok.clone() };
        assert!(!absurd.validate());
        let negative = StatsItem { durationMs: Some(-5), ..ok };
        assert!(!negative.validate());
    }

    #[test]
    fn fresh_event_id_is_deterministic_per_history_row() {
        let a = StatsItem::from_history_row("row-1", 1_000, "hello world", 500);
        let b = StatsItem::from_history_row("row-1", 1_000, "hello world", 500);
        assert_eq!(a.event_id, b.event_id, "duplicate commits dedup by construction");
        let c = StatsItem::from_history_row("row-2", 1_000, "hello world", 500);
        assert_ne!(a.event_id, c.event_id);
    }

    #[test]
    fn settings_unknown_keys_rejected_per_item() {
        let env = SettingsEnvelope {
            v: 1,
            items: vec![
                SettingsItem { key: "theme".into(), value: "dark".into(), updated_at: 1, device_id: "d".into() },
                SettingsItem { key: "language".into(), value: "en".into(), updated_at: 1, device_id: "d".into() },
            ],
        };
        let parsed = SettingsEnvelope::from_bytes(&env.to_bytes()).expect("parses");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].key, "language");
    }
}
