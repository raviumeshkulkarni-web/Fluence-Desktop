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

use serde::de::DeserializeOwned;
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
    // SATURATE, never truncate. `v as i32` wrapped: 2^32+1 became 1, so a
    // future envelope was misclassified as v1 — that is "corrupt", which the
    // sync pass is allowed to overwrite, destroying the newer client's data.
    value
        .get("v")?
        .as_i64()
        .map(|v| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
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

// ── Shared envelope decoding (audit #7) ─────────────────────────────────────
//
// The envelope SHAPE is decoded whole; each RECORD is decoded independently.
// Envelope-level problems (byte cap, malformed JSON, a wrong `v`, too many
// entries) reject the whole file, as the frozen contract requires. A record
// that fails to deserialize is skipped on its own — exactly what Android's
// twin does — so one structurally-malformed record can never cost the file
// its remaining records.
//
// This matters destructively: `sync_domain` sees an undecodable remote file
// as CORRUPT and, when local state is non-empty, PUTs a replacement over it.
// With whole-envelope decoding, a single missing `isEnabled` (or an
// `updatedAt` sent as a string instead of a number) was enough to wipe every
// record on Drive.
//
// `#[serde(default)]` on the item fields is deliberately NOT the fix: serde
// applies `default` only to a MISSING key, never to a wrong-typed one, and
// once defaulted it is impossible to tell an absent `isEnabled` from an
// explicit `false` — so the malformed record would be *admitted* rather than
// skipped. Decoding per record skips it outright and needs no field changes.

/// Envelope shell only: `{"v":<int>,"entries":[<raw record>, ...]}`.
#[derive(Deserialize)]
struct RawEnvelope {
    v: i64,
    #[serde(default)]
    entries: Vec<serde_json::Value>,
}

/// Decode one domain envelope with per-record isolation.
///
/// Returns `None` only for envelope-level problems. Individual records that
/// fail to deserialize, or that fail `is_valid`, are dropped individually.
fn decode_entries<T: DeserializeOwned>(
    bytes: &[u8],
    max_items: usize,
    is_valid: fn(&T) -> bool,
) -> Option<Vec<T>> {
    if bytes.len() > crate::sync::drive::MAX_DOMAIN_BYTES {
        return None;
    }
    let raw: RawEnvelope = serde_json::from_slice(bytes).ok()?;
    // `i64` not `i32`: an absurdly large `v` must not fail the parse here, so
    // that `is_future_envelope` still sees it and protects the file.
    if raw.v != i64::from(ENVELOPE_V1) || raw.entries.len() > max_items {
        return None;
    }
    Some(
        raw.entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value::<T>(entry).ok())
            .filter(|item| is_valid(item))
            .collect(),
    )
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
    /// A structurally-malformed record is skipped ON ITS OWN (audit #7).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            v: ENVELOPE_V1,
            entries: decode_entries(bytes, MAX_ENVELOPE_ITEMS, DictionaryItem::validate)?,
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
        Some(Self {
            v: ENVELOPE_V1,
            entries: decode_entries(bytes, MAX_ENVELOPE_ITEMS, SnippetItem::validate)?,
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
    /// Optional tombstone (audit #13). The frozen settings contract allows
    /// "+ optional deletedAt" so Android can retract a key it adopted
    /// (e.g. reset `ai_polish_style` to a default) without the other platform
    /// resurrecting its own value forever.
    ///
    /// It is OMITTED from the wire when `None`. That is load-bearing: the four
    /// canonical fixtures predate the field, and `skip_serializing_if` is what
    /// keeps them byte-identical on re-serialization (the byte-gate).
    ///
    /// Like every other domain, a tombstone is an ordinary LWW state: it wins
    /// on `updatedAt` and loses to any newer write. It is never compared as a
    /// tiebreak — see `merge::merge_settings`.
    #[serde(rename = "deletedAt", default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
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
        // Same tombstone sanity check every other domain applies, so a hostile
        // `deletedAt: 0` cannot masquerade as a retraction. Per-record skip.
        if let Some(d) = self.deleted_at {
            if d <= 0 {
                return false;
            }
        }
        // Char (code-point) cap, matching every other string cap here and the
        // contract's "settings value <=1024 chars". Bytes would reject any
        // value over ~342 CJK characters (audit #12).
        self.value.chars().count() <= 1024
    }

    /// True when this record is a retraction rather than a value write.
    pub fn is_tombstone(&self) -> bool {
        self.deleted_at.is_some()
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
        Some(Self {
            v: ENVELOPE_V1,
            entries: decode_entries(bytes, SETTINGS_KEYS.len() * 4, SettingsItem::validate)?,
        })
    }
}

// ── Stats ───────────────────────────────────────────────────────────────────
// Event-sourced union dedup by eventId. One event per completed dictation;
// totals are summed at display time so no event can ever be counted twice.
// Backfill of pre-sync history uses deterministic UUIDv5 ids derived from the
// history row id, making re-runs idempotent under union ded.
//
// Cross-platform note: Android's backfill uses a DIFFERENT derivation over a
// different source (the `stats_daily` aggregate, keyed by day+account+index),
// and that difference is deliberate and safe — see `synthetic_backfill_id`.
// Transcription history is device-local and never syncs, so one dictation is
// never backfilled on two devices and the union dedup is never asked to
// collapse a cross-platform pair. Changing either derivation would re-id every
// already-backfilled event and double the affected user's stats.

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

/// Deterministic UUIDv5 backfill id for legacy imports.
///
/// DEAD CODE — Windows backfills per history row via [`synthetic_event_id`], not
/// this. Kept only so the derivation stays documented next to the live one.
///
/// DO NOT "align" this with Android's backfill derivation, and do not read this
/// function as describing what Windows does. Android's backfill is over a
/// different source entirely (the `stats_daily` aggregate, keyed by
/// day+account+index), and the derivations differ deliberately: making them
/// match would be destructive, not a parity fix.
///
/// `Backfill.kt` carries the full reasoning, including the UNMITIGATED residual
/// that survives there — Android keys on a POSITIONAL index over the sorted day
/// list, so a day added or removed between runs re-ids later days. Changing
/// either derivation would re-id every already-backfilled event and double every
/// existing user's stats; see that file for why that trade is not worth making.
#[allow(dead_code)]
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
        Some(Self {
            v: ENVELOPE_V1,
            entries: decode_entries(bytes, MAX_ENVELOPE_ITEMS, StatsItem::validate)?,
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
    /// A structurally-malformed record is skipped ON ITS OWN (audit #7).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            v: ENVELOPE_V1,
            entries: decode_entries(bytes, MAX_ENVELOPE_ITEMS, AgentItem::validate)?,
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
        Some(Self {
            v: ENVELOPE_V1,
            entries: decode_entries(bytes, MAX_ENVELOPE_ITEMS, StyleItem::validate)?,
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
                    deleted_at: None,
                },
                SettingsItem {
                    key: "language".into(),
                    value: "en".into(),
                    updated_at: 1,
                    device_id: "d".into(),
                    deleted_at: None,
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

    /// #12 regression: the settings `value` cap is 1024 CHARACTERS (code
    /// points), exactly like every other string cap in this file. It was
    /// measured in UTF-8 BYTES, which rejected any value of ~342 CJK
    /// characters even though it is well inside the contract's 1024-char cap.
    #[test]
    fn settings_value_cap_counts_codepoints_not_utf8_bytes() {
        const CJK: char = '\u{65e5}'; // 日 - 3 UTF-8 bytes
        const EMOJI: char = '\u{1f600}'; // 😀 - 4 UTF-8 bytes (supplementary plane)
        let mut item = SettingsItem {
            key: "ai_polish_style".to_string(),
            value: CJK.to_string().repeat(1024),
            updated_at: 1,
            device_id: "d".to_string(),
            deleted_at: None,
        };
        assert_eq!(item.value.chars().count(), 1024);
        assert!(
            item.value.len() > 1024,
            "1024 CJK chars occupy {} UTF-8 bytes, well over the byte cap",
            item.value.len()
        );
        assert!(
            item.validate(),
            "1024 CJK code points is exactly at the cap and must be valid"
        );

        item.value = CJK.to_string().repeat(1025);
        assert!(!item.validate(), "1025 CJK code points exceeds the cap");

        item.value = EMOJI.to_string().repeat(1024);
        assert_eq!(item.value.chars().count(), 1024);
        assert_eq!(item.value.len(), 4096);
        assert!(
            item.validate(),
            "1024 astral-plane code points is exactly at the cap and must be valid"
        );

        item.value = "a".repeat(1024);
        assert!(
            item.validate(),
            "1024 ASCII characters stays valid (one byte per char)"
        );

        item.value = "a".repeat(1025);
        assert!(!item.validate(), "1025 ASCII characters exceeds the cap");
    }

    /// #7 regression: ONE structurally-malformed record must not cost the file.
    ///
    /// Before per-record decoding, `serde_json::from_slice` deserialized the
    /// WHOLE envelope at once, so a single record missing `isEnabled` (or one
    /// sending `updatedAt` as a string) made `from_bytes` return `None`. The
    /// sync pass reads `None` as CORRUPT and, with non-empty local state,
    /// PUTs a replacement over the file — destroying every other record on
    /// Drive. Android's twin skips only the bad record; this pins that parity.
    #[test]
    fn a_structurally_malformed_record_is_skipped_and_the_file_survives() {
        let bytes = br#"{"v":1,"entries":[
            {"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"keeper","spoken":"keeper","corrected":"keep me","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-a"},
            {"syncId":"22222222-2222-4222-8222-222222222222","businessKey":"noenabled","spoken":"noenabled","corrected":"missing isEnabled","updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-b"},
            {"syncId":"33333333-3333-4333-8333-333333333333","businessKey":"wrongtype","spoken":"wrongtype","corrected":"updatedAt is a string","isEnabled":true,"updatedAt":"1713471000123","deletedAt":null,"deviceId":"device-b"},
            {"businessKey":"nosyncid","spoken":"nosyncid","corrected":"no syncId","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-b"},
            "not even an object"
        ]}"#;
        let env = DictionaryEnvelope::from_bytes(bytes).expect("the file must still be readable");
        assert_eq!(
            env.entries
                .iter()
                .map(|e| e.spoken.as_str())
                .collect::<Vec<_>>(),
            vec!["keeper"],
            "only the four malformed records are dropped"
        );
        assert_eq!(
            env.entries[0].spoken, "keeper",
            "the one well-formed record must survive"
        );
    }

    /// Same parity for every other envelope type — the policy lives in
    /// `decode_entries`, and each domain must actually route through it.
    #[test]
    fn a_structurally_malformed_record_is_skipped_in_every_domain() {
        let snip = br#"{"v":1,"entries":[
            {"syncId":"11111111-1111-4111-8111-111111111111","businessKey":"ok","trigger":"ok","expansion":"kept","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-a"},
            {"syncId":"22222222-2222-4222-8222-222222222222","businessKey":"bad","trigger":"bad","isEnabled":true,"updatedAt":1713471000123,"deletedAt":null,"deviceId":"device-b"}
        ]}"#;
        let parsed = SnippetEnvelope::from_bytes(snip).expect("snippet envelope survives");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].trigger, "ok");

        let set = br#"{"v":1,"entries":[
            {"key":"language","value":"en","updatedAt":1713471000123,"deviceId":"device-a"},
            {"key":"language","value":"fr","updatedAt":1713471000123}
        ]}"#;
        let parsed = SettingsEnvelope::from_bytes(set).expect("settings envelope survives");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].value, "en");

        let stats = br#"{"v":1,"entries":[
            {"eventId":"11111111-1111-4111-8111-111111111111","day":"2026-08-20","timestampMs":1787184000123,"words":120,"chars":640,"durationMs":84000},
            {"eventId":"22222222-2222-4222-8222-222222222222","day":"2026-08-21","timestampMs":"nope"}
        ]}"#;
        let parsed = StatsEnvelope::from_bytes(stats).expect("stats envelope survives");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].day, "2026-08-20");

        let agent = br#"{"v":1,"entries":[
            {"syncId":"123e4567-e89b-12d3-a456-426614174000","businessKey":"agent:123e4567-e89b-12d3-a456-426614174001","name":"Translator","hint":"Be concise","updatedAt":1700000000000,"deletedAt":null,"deviceId":"dev-a"},
            {"syncId":"223e4567-e89b-12d3-a456-426614174000","name":"No businessKey","hint":"x","updatedAt":1700000000000,"deletedAt":null,"deviceId":"dev-a"}
        ]}"#;
        let parsed = AgentEnvelope::from_bytes(agent).expect("agent envelope survives");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].name, "Translator");

        let style = br#"{"v":1,"entries":[
            {"syncId":"123e4567-e89b-12d3-a456-426614174000","businessKey":"custom:123e4567-e89b-12d3-a456-426614174001","name":"Formal","hint":"Be formal","updatedAt":1700000000000,"deletedAt":null,"deviceId":"dev-a"},
            {"syncId":"223e4567-e89b-12d3-a456-426614174000","businessKey":"custom:123e4567-e89b-12d3-a456-426614174002","updatedAt":1700000000000,"deletedAt":null,"deviceId":"dev-a"}
        ]}"#;
        let parsed = StyleEnvelope::from_bytes(style).expect("style envelope survives");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].name, "Formal");
    }

    /// Envelope-LEVEL problems still reject the whole file — the per-record
    /// skip must not leak outward and start silently truncating a file whose
    /// shell itself is wrong.
    #[test]
    fn envelope_level_problems_still_reject_the_whole_file() {
        assert!(DictionaryEnvelope::from_bytes(b"{ not json").is_none());
        assert!(DictionaryEnvelope::from_bytes(br#"{"v":2,"entries":[]}"#).is_none());
        assert!(
            DictionaryEnvelope::from_bytes(br#"{"v":1}"#).is_some(),
            "absent entries default empty"
        );
        // `entries` must be an array; anything else is an envelope problem.
        assert!(DictionaryEnvelope::from_bytes(br#"{"v":1,"entries":{}}"#).is_none());
        assert!(DictionaryEnvelope::from_bytes(br#"{"v":1,"entries":null}"#).is_none());
        // Wrong type for `v` is an envelope problem too.
        assert!(DictionaryEnvelope::from_bytes(br#"{"v":"1","entries":[]}"#).is_none());
    }

    /// #7 regression: `v as i32` truncated, so 2^32+1 became exactly 1 and a
    /// future envelope was misread as v1 — i.e. "corrupt", which the sync pass
    /// is allowed to overwrite. A far-future envelope must read as future.
    #[test]
    fn declared_envelope_version_never_truncates_a_future_version_into_v1() {
        assert_eq!(
            declared_envelope_version(br#"{"v":4294967297}"#),
            Some(i32::MAX),
            "2^32+1 must saturate, not wrap back to 1"
        );
        assert!(
            is_future_envelope(br#"{"v":4294967297,"entries":[]}"#),
            "2^32+1 truncates to 1 under `as i32` and must still read as future"
        );
        assert!(is_future_envelope(br#"{"v":2,"entries":[]}"#));
        assert!(!is_future_envelope(br#"{"v":1,"entries":[]}"#));
        assert!(!is_future_envelope(br#"{"v":-1,"entries":[]}"#));
        // The far-future file is still unreadable to this client, and is
        // therefore fenced exactly like any other unsupported envelope.
        assert!(DictionaryEnvelope::from_bytes(br#"{"v":4294967297,"entries":[]}"#).is_none());
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
            deleted_at: None,
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
        let env = AgentEnvelope {
            v: 1,
            entries: vec![agent_fixture_item()],
        };
        assert_eq!(AGENT_FIXTURE.as_bytes(), env.to_bytes().as_slice());
        let parsed = AgentEnvelope::from_bytes(AGENT_FIXTURE.as_bytes()).expect("must parse");
        assert_eq!(vec![agent_fixture_item()], parsed.entries);
    }

    #[test]
    fn style_envelope_matches_the_cross_platform_fixture() {
        let env = StyleEnvelope {
            v: 1,
            entries: vec![style_fixture_item()],
        };
        assert_eq!(STYLE_FIXTURE.as_bytes(), env.to_bytes().as_slice());
        let parsed = StyleEnvelope::from_bytes(STYLE_FIXTURE.as_bytes()).expect("must parse");
        assert_eq!(vec![style_fixture_item()], parsed.entries);
    }

    #[test]
    fn agent_entries_serialize_sorted_by_business_key_then_sync_id() {
        let mut b = agent_fixture_item();
        b.business_key = "agent:223e4567-e89b-12d3-a456-426614174000".to_string();
        b.name = "Second".to_string();
        let env = AgentEnvelope {
            v: 1,
            entries: vec![b, agent_fixture_item()],
        };
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

        let forward = AgentEnvelope {
            v: 1,
            entries: vec![emoji.clone(), plain.clone()],
        };
        let backward = AgentEnvelope {
            v: 1,
            entries: vec![plain, emoji],
        };
        assert_eq!(
            forward.to_bytes(),
            backward.to_bytes(),
            "input order and name content must not change canonical output"
        );
    }
}
