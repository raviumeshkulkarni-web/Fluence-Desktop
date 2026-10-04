// Fluence sync - frozen v1.2 domain envelopes (dictionary, snippets, stats, settings)
//
// Drive: drive.appdata, appDataFolder/fluence/v1/{dictionary.json,snippets.json,stats.json,settings.json}
//
// Validation policy (v1.2 hardening):
// - Envelope version must be exactly 1; anything else is skipped as foreign.
// - Individual records that fail validation are SKIPPED, never applied -
//   one malformed record must not discard an otherwise-valid domain.
// - Envelopes exceeding the item cap or byte cap are rejected wholesale
//   (corruption/abuse guard).
// - businessKey identity is always recomputed from record CONTENT
//   (`business_key()` derives from spoken/trigger) - never trusted from wire.
// - Duplicate domain files on Drive are all fetched and merged by the engine.

use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub const ENVELOPE_V1: i32 = 1;

/// Envelope version declared by `bytes`, or `None` when the payload is not a
/// readable JSON object with an integer `v`.
///
/// This is what separates "a future client owns this file, do not touch it"
/// from "this file is corrupt, repair it". `from_bytes` returns `None` for
/// either cause, and repairing a future envelope destroys the newer client's
/// data.
pub fn declared_envelope_version(bytes: &[u8]) -> Option<i32> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("v")?.as_i64().map(|v| v as i32)
}

/// True when `bytes` is a readable envelope declaring a version newer than
/// [ENVELOPE_V1]. Mirrors Android's `DomainSerializer.isFutureEnvelope`.
pub fn is_future_envelope(bytes: &[u8]) -> bool {
    declared_envelope_version(bytes).is_some_and(|v| v > ENVELOPE_V1)
}

/// Maximum records accepted in one envelope. Legitimate accounts hold tens to
/// hundreds of dictionary words / snippets and thousands of stat events;
/// anything beyond this bound is corruption or abuse.
pub const MAX_ENVELOPE_ITEMS: usize = 50_000;

/// F3 - far-future clock cap: records stamped >24h beyond wall clock are invalid (per-record skip, never whole-file).
pub const CLOCK_SKEW_TOLERANCE_MS: i64 = 24 * 60 * 60 * 1000;

fn default_kind() -> String {
    "correction".to_string()
}

// ── Dictionary ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DictionaryItem {
    #[serde(rename = "syncId")]
    pub sync_id: String,
    pub spoken: String,
    pub corrected: String,
    #[serde(default = "default_kind", skip_serializing)]
    pub kind: String, // correction | expansion - internal only, never on wire (F1)
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
    #[serde(rename = "deletedAt")]
    pub deleted_at: Option<i64>,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

impl Serialize for DictionaryItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // F1 - strict wire contract: {syncId, businessKey, spoken, corrected, isEnabled, updatedAt, deletedAt, deviceId}
        // businessKey computed, kind never emitted, fixed order per frozen README
        let mut s = serializer.serialize_struct("DictionaryItem", 8)?;
        s.serialize_field("syncId", &self.sync_id)?;
        s.serialize_field("businessKey", &self.business_key())?;
        s.serialize_field("spoken", &self.spoken)?;
        s.serialize_field("corrected", &self.corrected)?;
        s.serialize_field("isEnabled", &self.is_enabled)?;
        s.serialize_field("updatedAt", &self.updated_at)?;
        s.serialize_field("deletedAt", &self.deleted_at)?;
        s.serialize_field("deviceId", &self.device_id)?;
        s.end()
    }
}

impl DictionaryItem {
    /// Canonical identity - ALWAYS derived from content, never from any wire
    /// field. NFC-normalized + case-insensitive on the spoken form (symmetric cross-platform).
    pub fn business_key(&self) -> String {
        self.spoken.trim().nfc().collect::<String>().to_lowercase()
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
        // F3 - far-future cap: reject records stamped >24h beyond wall clock (per-record skip, never whole-file)
        let now = chrono::Utc::now().timestamp_millis();
        if self.updated_at > now + CLOCK_SKEW_TOLERANCE_MS {
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
        self.spoken.chars().count() <= 4096 && self.corrected.chars().count() <= 4096
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DictionaryEnvelope {
    pub v: i32,
    #[serde(default)]
    pub entries: Vec<DictionaryItem>,
}

impl DictionaryEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// Lenient ingest: unknown fields ignored, invalid ITEMS skipped, only a
    /// wrong version or oversized item count poisons the whole envelope.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.entries.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            entries: env.entries.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

// ── Snippets ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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

impl Serialize for SnippetItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // F1 - strict wire contract: {syncId, businessKey, trigger, expansion, isEnabled, updatedAt, deletedAt, deviceId}
        let mut s = serializer.serialize_struct("SnippetItem", 8)?;
        s.serialize_field("syncId", &self.sync_id)?;
        s.serialize_field("businessKey", &self.business_key())?;
        s.serialize_field("trigger", &self.trigger)?;
        s.serialize_field("expansion", &self.expansion)?;
        s.serialize_field("isEnabled", &self.is_enabled)?;
        s.serialize_field("updatedAt", &self.updated_at)?;
        s.serialize_field("deletedAt", &self.deleted_at)?;
        s.serialize_field("deviceId", &self.device_id)?;
        s.end()
    }
}

impl SnippetItem {
    /// Canonical identity - derived from the trigger content. NFC-normalized + case-insensitive (symmetric).
    pub fn business_key(&self) -> String {
        self.trigger.trim().nfc().collect::<String>().to_lowercase()
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
        // F3 - far-future cap
        let now = chrono::Utc::now().timestamp_millis();
        if self.updated_at > now + CLOCK_SKEW_TOLERANCE_MS {
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
        self.trigger.chars().count() <= 4096 && self.expansion.chars().count() <= 8192
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnippetEnvelope {
    pub v: i32,
    #[serde(default)]
    pub entries: Vec<SnippetItem>,
}

impl SnippetEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.entries.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            entries: env.entries.into_iter().filter(|i| i.validate()).collect(),
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
        // F3 - far-future cap
        let now = chrono::Utc::now().timestamp_millis();
        if self.updated_at > now + CLOCK_SKEW_TOLERANCE_MS {
            return false;
        }
        self.value.len() <= 1024
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsEnvelope {
    pub v: i32,
    #[serde(default)]
    pub entries: Vec<SettingsItem>,
}

impl SettingsEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.entries.len() > SETTINGS_KEYS.len() * 4 {
            return None;
        }
        Some(Self {
            v: env.v,
            entries: env.entries.into_iter().filter(|i| i.validate()).collect(),
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
    pub day: String,
    #[serde(rename = "timestampMs", default)]
    pub timestamp_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<i64>,
    #[serde(
        rename = "durationMs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ms: Option<i64>,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(rename = "deviceId", default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
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
        if self.timestamp_ms < 0 {
            return false;
        }
        // F3 - far-future cap for stats: updatedAt > now+24h is invalid per-record (only Some, None stays valid)
        if let Some(t) = self.updated_at {
            let now = chrono::Utc::now().timestamp_millis();
            if t > now + CLOCK_SKEW_TOLERANCE_MS {
                return false;
            }
        }
        // Sanity: a single dictation cannot contribute absurd magnitudes.
        let words = self.words.unwrap_or(0);
        let chars = self.chars.unwrap_or(0);
        let dur = self.duration_ms.unwrap_or(0);
        (0..=1_000_000).contains(&words)
            && (0..=10_000_000).contains(&chars)
            && (0..=86_400_000 * 7).contains(&dur)
    }

    /// Fresh event for one completed dictation. `event_id` is deterministic
    /// per history row (UUIDv5 of the row id) so even a duplicated commit
    /// path produces the SAME event id and union dedup absorbs it - exactly
    /// once counting by construction.
    pub fn from_history_row(
        history_id: &str,
        timestamp_ms: i64,
        text: &str,
        duration_ms: i64,
    ) -> Self {
        let day = chrono::DateTime::from_timestamp_millis(timestamp_ms)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "1970-01-01".to_string());
        Self {
            event_id: synthetic_event_id(history_id),
            day,
            timestamp_ms,
            words: Some(text.split_whitespace().count() as i64),
            chars: Some(text.chars().count() as i64),
            duration_ms: Some(duration_ms),
            updated_at: None,
            device_id: None,
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

/// F2 - collapse rule primitive: filter day-aggregate StatsItems (timestamp_ms==0 && chars==0)
/// where (day) already has dictation-level events. Pure and testable; used by the post-merge
/// filter below AND keeps the flagged legacy reconciliation OFF (resurrection-unsafe).
pub fn filter_aggregates_for_existing_dictation(
    aggregates: Vec<StatsItem>,
    existing_dictation_days: &std::collections::HashSet<String>,
) -> Vec<StatsItem> {
    aggregates
        .into_iter()
        .filter(|a| !existing_dictation_days.contains(&a.day))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatsEnvelope {
    pub v: i32,
    #[serde(default)]
    pub entries: Vec<StatsItem>,
}

impl StatsEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.entries.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            entries: env.entries.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 6 — Agents / Styles (additive v1 domains, account-partitioned).
//
// Wire contract (byte-identical with Android `DomainSerializer`):
//   envelope: {"v":1,"entries":[...]} + trailing "\n"
//   entries sorted by (businessKey, syncId) at serialize time
//   record: {"syncId","businessKey","name","hint","updatedAt","deletedAt","deviceId"}
//   deletedAt is explicit null when absent (serde default for Option)
//   businessKey "agent:<uuid>" / "custom:<uuid>", UUID-validated, never derived
//
// Unlike dictionary/snippets, the business key here is NOT content-derived:
// it is the record's own stable id, assigned once at creation. Two devices
// that independently create agents cannot collide, and renaming never forks.
// The key is validated but never recomputed and never trusted blindly.
// ---------------------------------------------------------------------------

/// Field order is the wire order. Do not reorder: serde emits declaration
/// order, and byte parity with Android depends on it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentItem {
    pub sync_id: String,
    pub business_key: String,
    pub name: String,
    #[serde(default)]
    pub hint: String,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    pub device_id: String,
}

impl AgentItem {
    pub const PREFIX: &'static str = "agent:";

    pub fn is_valid_business_key(k: &str) -> bool {
        k.len() > Self::PREFIX.len()
            && k.starts_with(Self::PREFIX)
            && uuid::Uuid::parse_str(&k[Self::PREFIX.len()..]).is_ok()
    }

    /// Stable sync id for a record that has none, byte-identical with Android
    /// `stableSyncId`: `UUID.nameUUIDFromBytes("fluence-agent:" + id)`, i.e.
    /// MD5 of the raw UTF-8 bytes with UUIDv3 version/variant bits and NO
    /// namespace. (`uuid::new_v3` always prepends a namespace and would give a
    /// different value, so the digest is constructed manually.)
    ///
    /// Only fills ABSENT ids. A record stamped by either platform keeps its id,
    /// so this runs solely for legacy records that predate sync on a device.
    pub fn stable_sync_id(id: &str) -> String {
        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        hasher.update(format!("fluence-agent:{}", id).as_bytes());
        let mut bytes: [u8; 16] = hasher.finalize().into();
        bytes[6] = (bytes[6] & 0x0f) | 0x30;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }

    /// Per-record validation, mirroring Android `AgentRecord.isValid`.
    /// Invalid records are skipped at ingest. A tombstone (`deleted_at`
    /// present) is still a valid record: dropping it would resurrect the
    /// deleted agent on the next merge.
    pub fn validate(&self) -> bool {
        if uuid::Uuid::parse_str(&self.sync_id).is_err() {
            return false;
        }
        if !Self::is_valid_business_key(&self.business_key) {
            return false;
        }
        if self.device_id.is_empty() {
            return false;
        }
        if self.updated_at <= 0 {
            return false;
        }
        let now = chrono::Utc::now().timestamp_millis();
        if self.updated_at > now + CLOCK_SKEW_TOLERANCE_MS {
            return false;
        }
        if let Some(d) = self.deleted_at {
            if d < 0 {
                return false;
            }
        }
        if self.name.trim().is_empty() {
            return false;
        }
        // Android counts Unicode code points, not bytes or UTF-16 units.
        if self.name.chars().count() > 1024 || self.hint.chars().count() > 8192 {
            return false;
        }
        true
    }
}

/// Field order is the wire order (see [`AgentItem`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StyleItem {
    pub sync_id: String,
    pub business_key: String,
    pub name: String,
    #[serde(default)]
    pub hint: String,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    pub device_id: String,
}

impl StyleItem {
    pub const PREFIX: &'static str = "custom:";

    pub fn is_valid_business_key(k: &str) -> bool {
        k.len() > Self::PREFIX.len()
            && k.starts_with(Self::PREFIX)
            && uuid::Uuid::parse_str(&k[Self::PREFIX.len()..]).is_ok()
    }

    /// Stable sync id, byte-identical with Android (`"fluence-style:"` tag).
    /// See [`AgentItem::stable_sync_id`] for the construction and the reason
    /// it must not use `uuid::new_v3`.
    pub fn stable_sync_id(id: &str) -> String {
        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        hasher.update(format!("fluence-style:{}", id).as_bytes());
        let mut bytes: [u8; 16] = hasher.finalize().into();
        bytes[6] = (bytes[6] & 0x0f) | 0x30;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }

    /// Identical rules to [`AgentItem::validate`].
    pub fn validate(&self) -> bool {
        if uuid::Uuid::parse_str(&self.sync_id).is_err() {
            return false;
        }
        if !Self::is_valid_business_key(&self.business_key) {
            return false;
        }
        if self.device_id.is_empty() {
            return false;
        }
        if self.updated_at <= 0 {
            return false;
        }
        let now = chrono::Utc::now().timestamp_millis();
        if self.updated_at > now + CLOCK_SKEW_TOLERANCE_MS {
            return false;
        }
        if let Some(d) = self.deleted_at {
            if d < 0 {
                return false;
            }
        }
        if self.name.trim().is_empty() {
            return false;
        }
        if self.name.chars().count() > 1024 || self.hint.chars().count() > 8192 {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentEnvelope {
    pub v: i32,
    #[serde(default)]
    pub entries: Vec<AgentItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StyleEnvelope {
    pub v: i32,
    #[serde(default)]
    pub entries: Vec<StyleItem>,
}

fn sort_agent_entries(entries: &mut [AgentItem]) {
    entries.sort_by(|a, b| {
        a.business_key
            .cmp(&b.business_key)
            .then_with(|| a.sync_id.cmp(&b.sync_id))
    });
}

fn sort_style_entries(entries: &mut [StyleItem]) {
    entries.sort_by(|a, b| {
        a.business_key
            .cmp(&b.business_key)
            .then_with(|| a.sync_id.cmp(&b.sync_id))
    });
}

impl AgentEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut sorted = self.clone();
        sort_agent_entries(&mut sorted.entries);
        let mut bytes = serde_json::to_vec(&sorted).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// Lenient ingest: unknown fields ignored, invalid ITEMS skipped, only a
    /// wrong version or oversized item count poisons the whole envelope.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.entries.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            entries: env.entries.into_iter().filter(|i| i.validate()).collect(),
        })
    }
}

impl StyleEnvelope {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut sorted = self.clone();
        sort_style_entries(&mut sorted.entries);
        let mut bytes = serde_json::to_vec(&sorted).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// Lenient ingest (see [`AgentEnvelope::from_bytes`]).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
            return None;
        }
        let env: Self = serde_json::from_slice(bytes).ok()?;
        if env.v != ENVELOPE_V1 || env.entries.len() > MAX_ENVELOPE_ITEMS {
            return None;
        }
        Some(Self {
            v: env.v,
            entries: env.entries.into_iter().filter(|i| i.validate()).collect(),
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
        let env = DictionaryEnvelope {
            v: 1,
            entries: vec![good, bad],
        };
        let bytes = env.to_bytes();
        let parsed = DictionaryEnvelope::from_bytes(&bytes).expect("envelope parses");
        assert_eq!(parsed.entries.len(), 1, "invalid item skipped, valid kept");
        assert_eq!(parsed.entries[0].spoken, "hello");
    }

    #[test]
    fn wrong_version_rejects_whole_envelope() {
        let env = DictionaryEnvelope {
            v: 2,
            entries: vec![dict("a", "b", 1)],
        };
        assert!(DictionaryEnvelope::from_bytes(&env.to_bytes()).is_none());
    }

    #[test]
    fn oversized_item_count_rejects_envelope() {
        let items: Vec<DictionaryItem> = (0..MAX_ENVELOPE_ITEMS + 1)
            .map(|i| dict(&format!("w{i}"), "x", 1))
            .collect();
        let env = DictionaryEnvelope {
            v: 1,
            entries: items,
        };
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
            duration_ms: Some(60_000),
            updated_at: None,
            device_id: None,
        };
        assert!(ok.validate());
        let absurd = StatsItem {
            words: Some(99_000_000),
            ..ok.clone()
        };
        assert!(!absurd.validate());
        let negative = StatsItem {
            duration_ms: Some(-5),
            ..ok
        };
        assert!(!negative.validate());
    }

    #[test]
    fn fresh_event_id_is_deterministic_per_history_row() {
        let a = StatsItem::from_history_row("row-1", 1_000, "hello world", 500);
        let b = StatsItem::from_history_row("row-1", 1_000, "hello world", 500);
        assert_eq!(
            a.event_id, b.event_id,
            "duplicate commits dedup by construction"
        );
        let c = StatsItem::from_history_row("row-2", 1_000, "hello world", 500);
        assert_ne!(a.event_id, c.event_id);
    }

    #[test]
    fn settings_unknown_keys_rejected_per_item() {
        let env = SettingsEnvelope {
            v: 1,
            entries: vec![
                SettingsItem {
                    key: "theme".into(),
                    value: "dark".into(),
                    updated_at: 1,
                    device_id: "d".into(),
                },
                SettingsItem {
                    key: "language".into(),
                    value: "en".into(),
                    updated_at: 1,
                    device_id: "d".into(),
                },
            ],
        };
        let parsed = SettingsEnvelope::from_bytes(&env.to_bytes()).expect("parses");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].key, "language");
    }

    #[test]
    fn android_canonical_fixtures_parse_and_roundtrip() {
        // UNIT A - byte-gate for all four domains against canonical fixtures (frozen contract).
        // Verifies: parse ok, counts, validation, and byte-identical re-serialization (exactly one trailing \n, CRLF-normalized).
        let dict_raw = include_bytes!("../../../examples/sync/v1/dictionary.json");
        let dict = DictionaryEnvelope::from_bytes(dict_raw).expect("dict fixture");
        assert_eq!(dict.entries.len(), 3);
        // businessKey-then-syncId canonical order per frozen README: asap < gonna < teh
        assert_eq!(dict.entries[0].business_key(), "asap");
        assert_eq!(dict.entries[1].business_key(), "gonna");
        assert_eq!(dict.entries[2].business_key(), "teh");
        // F1 - strict byte-gate: dict/snips must be byte-identical to fixtures (businessKey, no kind, fixed order)
        let dict_bytes = DictionaryEnvelope {
            v: 1,
            entries: dict.entries.clone(),
        }
        .to_bytes();
        let dict_raw_lf: Vec<u8> = dict_raw.iter().copied().filter(|b| *b != b'\r').collect();
        assert_eq!(
            dict_bytes.as_slice(),
            dict_raw_lf.as_slice(),
            "canonical dict bytes must be stable"
        );

        let snip_raw = include_bytes!("../../../examples/sync/v1/snippets.json");
        let snip = SnippetEnvelope::from_bytes(snip_raw).expect("snippet fixture");
        assert_eq!(snip.entries.len(), 2);
        assert_eq!(snip.entries[0].business_key(), "addr");
        assert_eq!(snip.entries[1].business_key(), "brb");
        let snip_bytes = SnippetEnvelope {
            v: 1,
            entries: snip.entries.clone(),
        }
        .to_bytes();
        let snip_raw_lf: Vec<u8> = snip_raw.iter().copied().filter(|b| *b != b'\r').collect();
        assert_eq!(
            snip_bytes.as_slice(),
            snip_raw_lf.as_slice(),
            "canonical snippet bytes must be stable"
        );

        let set_raw = include_bytes!("../../../examples/sync/v1/settings.json");
        let set = SettingsEnvelope::from_bytes(set_raw).expect("settings fixture");
        assert_eq!(set.entries.len(), 5);
        // key-sorted per contract
        assert_eq!(
            set.entries
                .iter()
                .map(|e| e.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "ai_polish_style",
                "auto_learn_enabled",
                "dictionary_enabled",
                "language",
                "snippets_enabled"
            ]
        );
        let set_bytes = SettingsEnvelope {
            v: 1,
            entries: set.entries.clone(),
        }
        .to_bytes();
        let set_raw_lf: Vec<u8> = set_raw.iter().copied().filter(|b| *b != b'\r').collect();
        assert_eq!(
            set_bytes.as_slice(),
            set_raw_lf.as_slice(),
            "canonical settings bytes must be stable"
        );

        let raw = include_bytes!("../../../examples/sync/v1/stats.json");
        let stats = StatsEnvelope::from_bytes(raw).expect("stats fixture");
        assert_eq!(stats.entries.len(), 2);
        assert!(stats
            .entries
            .iter()
            .any(|s| s.updated_at.is_none() && s.device_id.is_none()));
        let first = stats
            .entries
            .iter()
            .find(|s| s.event_id == "5f0c1a2b-3c4d-5e6f-8a9b-0c1d2e3f4a5b")
            .unwrap();
        assert_eq!(first.timestamp_ms, 1787184000123);
        assert!(stats.entries.iter().all(|s| s.validate()));

        let bytes = StatsEnvelope {
            v: 1,
            entries: stats.entries.clone(),
        }
        .to_bytes();
        let raw_lf: Vec<u8> = raw.iter().copied().filter(|b| *b != b'\r').collect();
        assert_eq!(
            bytes.as_slice(),
            raw_lf.as_slice(),
            "canonical stats bytes must be stable"
        );
    }

    #[test]
    fn emoji_length_counts_codepoints_not_bytes() {
        let mut item = dict("a", "b", 100);
        item.spoken = "😀".repeat(3000);
        assert_eq!(item.spoken.chars().count(), 3000);
        assert!(item.spoken.len() > 6000);
        assert!(
            item.validate(),
            "3000-emoji spoken should pass codepoint cap"
        );
        let mut too_big = dict("a", "b", 100);
        too_big.spoken = "😀".repeat(4097);
        assert!(!too_big.validate(), "4097 chars should fail");
    }

    #[test]
    fn business_key_nfc_normalized_cafe() {
        // ITEM 1 - NFC: precomposed é (U+00E9) vs e + combining acute (U+0301) must yield same businessKey, merge dedups to one
        let precomposed = "café"; // café with U+00E9
        let decomposed = "cafe\u{0301}"; // cafe + U+0301
        let a = dict(precomposed, "x", 100);
        let b = dict(decomposed, "y", 100);
        assert_eq!(
            a.business_key(),
            b.business_key(),
            "NFC must normalize café variants"
        );
        // Also snippet
        let s1 = SnippetItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            trigger: precomposed.to_string(),
            expansion: "exp".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 100,
            device_id: "d".to_string(),
        };
        let s2 = SnippetItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            trigger: decomposed.to_string(),
            expansion: "exp2".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: 200,
            device_id: "d".to_string(),
        };
        assert_eq!(s1.business_key(), s2.business_key());
        // Merge yields one winner (newer wins but only one key)
        let m = crate::sync::merge::merge_dictionary(&[a.clone()], &[b.clone()]);
        assert_eq!(
            m.merged.len(),
            1,
            "NFC variants must collapse to one businessKey"
        );
        let ms = crate::sync::merge::merge_snippets(&[s1.clone()], &[s2.clone()]);
        assert_eq!(ms.merged.len(), 1);
    }

    #[test]
    fn future_stamped_record_skipped_per_record() {
        // F3 - far-future cap: updatedAt > now+24h is invalid per-record, never whole-file
        let now = chrono::Utc::now().timestamp_millis();
        let future = now + CLOCK_SKEW_TOLERANCE_MS + 60_000;
        let mut bad = dict("future", "bad", future);
        // future must be rejected
        assert!(!bad.validate(), "future stamp must be skipped");
        // valid past still passes
        let mut good = dict("hello", "hi", now - 1000);
        assert!(good.validate());
        // Envelope with one future + one valid: future skipped, valid kept, whole file parses
        let env = DictionaryEnvelope {
            v: 1,
            entries: vec![good.clone(), bad.clone()],
        };
        let bytes = env.to_bytes();
        let parsed = DictionaryEnvelope::from_bytes(&bytes).expect("envelope must parse");
        assert_eq!(
            parsed.entries.len(),
            1,
            "future record must be skipped, valid kept"
        );
        assert_eq!(parsed.entries[0].spoken, "hello");
        // Snippet future also skipped
        let mut s_bad = SnippetItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            trigger: "trig".to_string(),
            expansion: "exp".to_string(),
            is_enabled: true,
            deleted_at: None,
            updated_at: future,
            device_id: "d".to_string(),
        };
        assert!(!s_bad.validate());
        // Settings future also skipped
        let mut set_bad = SettingsItem {
            key: "language".to_string(),
            value: "en".to_string(),
            updated_at: future,
            device_id: "d".to_string(),
        };
        assert!(!set_bad.validate());
        // Stats future also skipped, None stays valid
        let stats_bad = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 0,
            words: Some(10),
            chars: Some(100),
            duration_ms: Some(1000),
            updated_at: Some(future),
            device_id: Some("d".to_string()),
        };
        assert!(
            !stats_bad.validate(),
            "stats future updatedAt must be skipped"
        );
        let stats_good = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 0,
            words: Some(10),
            chars: Some(100),
            duration_ms: Some(1000),
            updated_at: Some(now - 1000),
            device_id: Some("d".to_string()),
        };
        assert!(stats_good.validate());
        let stats_none = StatsItem {
            event_id: uuid::Uuid::new_v4().to_string(),
            day: "2026-08-20".to_string(),
            timestamp_ms: 0,
            words: Some(10),
            chars: Some(100),
            duration_ms: Some(1000),
            updated_at: None,
            device_id: None,
        };
        assert!(stats_none.validate(), "None updatedAt must remain valid");
        let env_stats = StatsEnvelope {
            v: 1,
            entries: vec![stats_good.clone(), stats_bad.clone(), stats_none.clone()],
        };
        let bytes_stats = env_stats.to_bytes();
        let parsed_stats =
            StatsEnvelope::from_bytes(&bytes_stats).expect("stats envelope must parse");
        assert_eq!(
            parsed_stats.entries.len(),
            2,
            "future stats skipped, valid+None kept"
        );
    }
}

#[cfg(test)]
mod agent_style_conformance_tests {
    use super::*;

    /// Byte-identical wire vector shared with Android
    /// (`AgentStyleDomainSyncTest.agent_envelope_matches_the_cross_platform_fixture`
    /// asserts the same literal). Any divergence in field order, escaping,
    /// sorting, the trailing newline, or null-vs-absent encoding fails here
    /// rather than surfacing as cross-device data loss.
    const AGENT_FIXTURE: &str = "{\"v\":1,\"entries\":[{\"syncId\":\"123e4567-e89b-12d3-a456-426614174000\",\"businessKey\":\"agent:123e4567-e89b-12d3-a456-426614174001\",\"name\":\"Translator\",\"hint\":\"Be concise\",\"updatedAt\":1700000000000,\"deletedAt\":null,\"deviceId\":\"dev-a\"}]}\n";

    const STYLE_FIXTURE: &str = "{\"v\":1,\"entries\":[{\"syncId\":\"123e4567-e89b-12d3-a456-426614174000\",\"businessKey\":\"custom:123e4567-e89b-12d3-a456-426614174001\",\"name\":\"Formal\",\"hint\":\"Be formal\",\"updatedAt\":1700000000000,\"deletedAt\":null,\"deviceId\":\"dev-a\"}]}\n";

    fn agent_fixture_item() -> AgentItem {
        AgentItem {
            sync_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            business_key: "agent:123e4567-e89b-12d3-a456-426614174001".to_string(),
            name: "Translator".to_string(),
            hint: "Be concise".to_string(),
            updated_at: 1700000000000,
            deleted_at: None,
            device_id: "dev-a".to_string(),
        }
    }

    fn style_fixture_item() -> StyleItem {
        StyleItem {
            sync_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            business_key: "custom:123e4567-e89b-12d3-a456-426614174001".to_string(),
            name: "Formal".to_string(),
            hint: "Be formal".to_string(),
            updated_at: 1700000000000,
            deleted_at: None,
            device_id: "dev-a".to_string(),
        }
    }

    #[test]
    fn agent_envelope_matches_the_cross_platform_fixture() {
        let env = AgentEnvelope { v: 1, entries: vec![agent_fixture_item()] };
        assert_eq!(AGENT_FIXTURE.as_bytes(), env.to_bytes().as_slice());
        let parsed = AgentEnvelope::from_bytes(AGENT_FIXTURE.as_bytes()).expect("must parse");
        assert_eq!(vec![agent_fixture_item()], parsed.entries);
    }

    #[test]
    fn style_envelope_matches_the_cross_platform_fixture() {
        let env = StyleEnvelope { v: 1, entries: vec![style_fixture_item()] };
        assert_eq!(STYLE_FIXTURE.as_bytes(), env.to_bytes().as_slice());
        let parsed = StyleEnvelope::from_bytes(STYLE_FIXTURE.as_bytes()).expect("must parse");
        assert_eq!(vec![style_fixture_item()], parsed.entries);
    }

    #[test]
    fn agent_entries_serialize_sorted_by_business_key_then_sync_id() {
        let mut b = agent_fixture_item();
        b.business_key = "agent:223e4567-e89b-12d3-a456-426614174000".to_string();
        b.name = "Second".to_string();
        let env = AgentEnvelope { v: 1, entries: vec![b, agent_fixture_item()] };
        let bytes = env.to_bytes();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.find("Translator").unwrap() < s.find("Second").unwrap());
    }

    #[test]
    fn agent_validation_rejects_bad_keys_and_accepts_tombstones() {
        assert!(agent_fixture_item().validate());
        let mut bad_prefix = agent_fixture_item();
        bad_prefix.business_key = "dictionary:hello".to_string();
        assert!(!bad_prefix.validate());
        let mut bad_uuid = agent_fixture_item();
        bad_uuid.business_key = "agent:not-a-uuid".to_string();
        assert!(!bad_uuid.validate());
        let mut tomb = agent_fixture_item();
        tomb.deleted_at = Some(1700000000001);
        assert!(tomb.validate(), "tombstones must survive ingest");
        let mut neg_del = agent_fixture_item();
        neg_del.deleted_at = Some(-1);
        assert!(!neg_del.validate());
    }

    #[test]
    fn style_validation_mirrors_agent_validation() {
        assert!(style_fixture_item().validate());
        let mut bad = style_fixture_item();
        bad.business_key = "agent:123e4567-e89b-12d3-a456-426614174001".to_string();
        assert!(!bad.validate(), "agent keys must not pass as styles");
    }

    #[test]
    fn corrupt_and_future_envelopes_are_rejected_wholesale() {
        assert!(AgentEnvelope::from_bytes(b"not json").is_none());
        assert!(AgentEnvelope::from_bytes(b"{\"v\":2,\"entries\":[]}").is_none());
        assert!(StyleEnvelope::from_bytes(b"{\"v\":99,\"entries\":[]}").is_none());
    }

    #[test]
    fn stable_sync_ids_match_the_cross_platform_fixtures() {
        // Independently computed (MD5 + UUIDv3 bits, no namespace) and asserted
        // identically on Android. A mismatch here splits LWW tie-breaks across
        // platforms for legacy records stamped independently on each side.
        assert_eq!(
            "c8e4002e-0a78-3e24-9c34-143a05e25777",
            AgentItem::stable_sync_id("agent:123e4567-e89b-12d3-a456-426614174001")
        );
        assert_eq!(
            "d7848828-627e-3a3e-b1c4-45d605f06e44",
            StyleItem::stable_sync_id("custom:123e4567-e89b-12d3-a456-426614174001")
        );
    }

    /// Cross-platform ordering invariant.
    ///
    /// Kotlin `String.compareTo` is UTF-16 code-unit order; Rust `str::cmp` is
    /// UTF-8 byte order. The two coincide for ASCII and DIVERGE for
    /// supplementary-plane characters (surrogate pairs vs 4-byte sequences).
    /// The canonical sort keys — `businessKey` = prefix + UUID, `syncId` = UUID
    /// — are ASCII by construction, and this test pins that constraint so a
    /// future move to name-derived identity cannot silently break
    /// cross-platform convergence.
    #[test]
    fn canonical_sort_keys_are_ascii_so_platform_ordering_coincides() {
        let a = agent_fixture_item();
        assert!(a.business_key.is_ascii(), "businessKey must stay ASCII");
        assert!(a.sync_id.is_ascii(), "syncId must stay ASCII");
        let s = style_fixture_item();
        assert!(s.business_key.is_ascii(), "businessKey must stay ASCII");
        assert!(s.sync_id.is_ascii(), "syncId must stay ASCII");
        // The generated stable id is a UUID: ASCII hex and dashes.
        assert!(AgentItem::stable_sync_id("agent:whatever").is_ascii());
    }

    /// Names are free text and are NEVER sort keys, so a non-ASCII (emoji) name
    /// must not affect ordering or output bytes for a given key set.
    #[test]
    fn non_ascii_names_do_not_influence_ordering() {
        let mut emoji = agent_fixture_item();
        emoji.name = "\u{1F389} agent".to_string();
        let mut plain = agent_fixture_item();
        plain.business_key = "agent:323e4567-e89b-12d3-a456-426614174000".to_string();
        plain.sync_id = "223e4567-e89b-12d3-a456-426614174000".to_string();
        plain.name = "zzz".to_string();

        let forward = AgentEnvelope { v: 1, entries: vec![emoji.clone(), plain.clone()] };
        let backward = AgentEnvelope { v: 1, entries: vec![plain, emoji] };
        assert_eq!(
            forward.to_bytes(),
            backward.to_bytes(),
            "input order and name content must not change canonical output"
        );
    }
}
