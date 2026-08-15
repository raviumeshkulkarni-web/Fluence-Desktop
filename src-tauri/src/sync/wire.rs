// Fluence sync — wire codec (spec §4, §30).
//
// Schema v1 JSON records stored on Google Drive, one file per record:
// `<uuid-v4>.json`. This module only encodes/decodes; reconciliation lives in
// `engine.rs`. §30 adds an optional `type` discriminator (`history` default),
// so history records are byte-identical to the Phase 1–4 format.

use serde::{Deserialize, Serialize};

/// Record kind (§30.1). `History` is the default; the field is additive, so
/// records without `type` parse as history and serialize without the field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordType {
    #[serde(rename = "history")]
    #[default]
    History,
    #[serde(rename = "dictionary")]
    Dictionary,
    #[serde(rename = "snippet")]
    Snippet,
    #[serde(rename = "settings")]
    Settings,
}

impl RecordType {
    pub fn is_history(&self) -> bool {
        *self == RecordType::History
    }
}

/// A schema-v1 wire record, mirroring `examples/sync/*.json`.
///
/// Field order is declaration order: `to_json` always serializes compactly in
/// this exact order, which makes byte-level equality deterministic. History
/// records omit every §30 field (skipped when empty/default), so their bytes
/// are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRecord {
    pub v: i32,
    pub id: String,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
    #[serde(
        default,
        rename = "type",
        skip_serializing_if = "RecordType::is_history"
    )]
    pub rtype: RecordType,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub provider: String,
    pub model: Option<String>,
    pub language: Option<String>,
    // §30 content fields — history records never carry them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<String>,
    #[serde(default, rename = "key", skip_serializing_if = "Option::is_none")]
    pub settings_key: Option<String>,
    #[serde(default, rename = "value", skip_serializing_if = "Option::is_none")]
    pub settings_value: Option<String>,
}

/// Content tuple `T = (created_at, text, mode, duration_ms, provider, model,
/// language)` for `history` records. `deleted_at` is deliberately NOT part of
/// `T` (spec §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentTuple {
    pub created_at: i64,
    pub text: String,
    pub mode: String,
    pub duration_ms: i64,
    pub provider: String,
    pub model: Option<String>,
    pub language: Option<String>,
}

/// Content tuple for `dictionary` records (§30.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryTuple {
    pub created_at: i64,
    pub spoken: String,
    pub corrected: String,
    pub kind: String,
}

/// Content tuple for `snippet` records (§30.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetTuple {
    pub created_at: i64,
    pub trigger: String,
    pub expansion: String,
}

/// Content tuple for `settings` records (§30.1, §30.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsTuple {
    pub created_at: i64,
    pub key: String,
    pub value: String,
}

/// Kind-aware content of a record — the equality domain for group
/// classification (§9). Two records of different kinds are never equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordContent {
    History(ContentTuple),
    Dictionary(DictionaryTuple),
    Snippet(SnippetTuple),
    Settings(SettingsTuple),
}

impl WireRecord {
    pub fn content(&self) -> RecordContent {
        match self.rtype {
            RecordType::History => RecordContent::History(ContentTuple {
                created_at: self.created_at,
                text: self.text.clone(),
                mode: self.mode.clone(),
                duration_ms: self.duration_ms,
                provider: self.provider.clone(),
                model: self.model.clone(),
                language: self.language.clone(),
            }),
            RecordType::Dictionary => RecordContent::Dictionary(DictionaryTuple {
                created_at: self.created_at,
                spoken: self.spoken.clone().unwrap_or_default(),
                corrected: self.corrected.clone().unwrap_or_default(),
                kind: self.kind.clone().unwrap_or_default(),
            }),
            RecordType::Snippet => RecordContent::Snippet(SnippetTuple {
                created_at: self.created_at,
                trigger: self.trigger.clone().unwrap_or_default(),
                expansion: self.expansion.clone().unwrap_or_default(),
            }),
            RecordType::Settings => RecordContent::Settings(SettingsTuple {
                created_at: self.created_at,
                key: self.settings_key.clone().unwrap_or_default(),
                value: self.settings_value.clone().unwrap_or_default(),
            }),
        }
    }

    /// Compact, deterministic JSON (declaration order). Infallible for this
    /// all-primitive struct.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("WireRecord serialization cannot fail")
    }
}

/// Exact field equality on `T` — no equivalence, no canonicalization (R1).
pub fn tuples_equal(a: &RecordContent, b: &RecordContent) -> bool {
    a == b
}

/// Full record with the same `T` and `deleted_at` set (matches fixture ...003).
pub fn tombstone(record: &WireRecord, deleted_at: i64) -> WireRecord {
    let mut out = record.clone();
    out.deleted_at = Some(deleted_at);
    out
}

/// Why a file failed validation. Every reason maps to a quarantine reason in
/// the engine (spec §4, §9, §30.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidReason {
    MalformedJson,
    UnknownSchemaVersion,
    IdNameMismatch,
    BadTimestamp,
    BadMode,
    NonIntegral,
    UnknownType,
    MissingTypeField,
    BadKind,
}

fn parse_record_type(value: &serde_json::Value) -> Result<RecordType, InvalidReason> {
    match value.get("type") {
        None | Some(serde_json::Value::Null) => Ok(RecordType::History),
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "history" => Ok(RecordType::History),
            "dictionary" => Ok(RecordType::Dictionary),
            "snippet" => Ok(RecordType::Snippet),
            "settings" => Ok(RecordType::Settings),
            _ => Err(InvalidReason::UnknownType),
        },
        Some(_) => Err(InvalidReason::UnknownType),
    }
}

fn field_present_and_nonblank(value: &serde_json::Value, key: &str) -> bool {
    match value.get(key) {
        Some(serde_json::Value::String(s)) => !s.trim().is_empty(),
        _ => false,
    }
}

/// Validate `bytes` against the schema-v1 rules (spec §4, §30.1):
/// `v == 1`, lowercase UUID v4 `id` equal to `basename`, `created_at > 0`,
/// `deleted_at` null or positive, all ints are ints; then per type — history
/// requires `text`/`mode`/`duration_ms`/`provider` present and mode in the two
/// known values; `dictionary` requires `spoken`/`corrected` (non-blank) and
/// `kind ∈ {correction, expansion}`; `snippet` requires `trigger`/`expansion`;
/// `settings` requires `key`. Unknown `type` → `UnknownType`. `basename` is
/// the UUID stem of the file name (no `.json`).
pub fn parse(bytes: &[u8], basename: &str) -> Result<WireRecord, InvalidReason> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| InvalidReason::MalformedJson)?;

    let rtype = parse_record_type(&value)?;

    check_integral(&value, "v")?;
    check_integral(&value, "created_at")?;
    check_optional_integral(&value, "deleted_at")?;

    // Per-type presence/integrality checks on the raw JSON (the struct uses
    // serde defaults so non-history kinds may omit the history-only fields).
    match rtype {
        RecordType::History => {
            check_integral(&value, "duration_ms")?;
            if value.get("text").is_none()
                || value.get("mode").is_none()
                || value.get("provider").is_none()
            {
                return Err(InvalidReason::MissingTypeField);
            }
        }
        RecordType::Dictionary => {
            if !field_present_and_nonblank(&value, "spoken")
                || !field_present_and_nonblank(&value, "corrected")
            {
                return Err(InvalidReason::MissingTypeField);
            }
        }
        RecordType::Snippet => {
            if !field_present_and_nonblank(&value, "trigger")
                || !field_present_and_nonblank(&value, "expansion")
            {
                return Err(InvalidReason::MissingTypeField);
            }
        }
        RecordType::Settings => {
            if !field_present_and_nonblank(&value, "key") {
                return Err(InvalidReason::MissingTypeField);
            }
        }
    }

    let mut record: WireRecord =
        serde_json::from_value(value).map_err(|_| InvalidReason::MalformedJson)?;
    record.rtype = rtype;

    if record.v != 1 {
        return Err(InvalidReason::UnknownSchemaVersion);
    }
    if record.id != basename || !is_lowercase_uuid_v4(&record.id) {
        return Err(InvalidReason::IdNameMismatch);
    }
    if record.created_at <= 0 {
        return Err(InvalidReason::BadTimestamp);
    }
    if record.deleted_at.is_some_and(|d| d <= 0) {
        return Err(InvalidReason::BadTimestamp);
    }
    match rtype {
        RecordType::History => {
            if record.mode != "transcription" && record.mode != "agent" {
                return Err(InvalidReason::BadMode);
            }
        }
        RecordType::Dictionary => match record.kind.as_deref() {
            Some("correction") | Some("expansion") => {}
            _ => return Err(InvalidReason::BadKind),
        },
        RecordType::Snippet | RecordType::Settings => {}
    }
    Ok(record)
}

fn check_integral(value: &serde_json::Value, key: &str) -> Result<(), InvalidReason> {
    match value.get(key) {
        Some(serde_json::Value::Number(n)) if n.is_i64() => Ok(()),
        _ => Err(InvalidReason::NonIntegral),
    }
}

fn check_optional_integral(value: &serde_json::Value, key: &str) -> Result<(), InvalidReason> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(()),
        Some(serde_json::Value::Number(n)) if n.is_i64() => Ok(()),
        _ => Err(InvalidReason::NonIntegral),
    }
}

fn is_lowercase_uuid_v4(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            if c != b'-' {
                return false;
            }
        } else if !matches!(c, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
    }
    b[14] == b'4' && matches!(b[19], b'8' | b'9' | b'a' | b'b')
}

/// The UUID stem of a Drive file name, if the name is a lowercase UUID v4
/// followed by `.json`. Non-matching names are inert (never fetched, never
/// imported — spec §15 "rename").
pub(crate) fn uuid_basename(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".json")?;
    if is_lowercase_uuid_v4(stem) {
        Some(stem)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F1: &str = "00000000-0000-4000-8000-000000000001";
    const F2: &str = "00000000-0000-4000-8000-000000000002";
    const F3: &str = "00000000-0000-4000-8000-000000000003";
    const F4: &str = "00000000-0000-4000-8000-000000000004";
    const F5: &str = "00000000-0000-4000-8000-000000000005";
    const F6: &str = "00000000-0000-4000-8000-000000000006";
    const F7: &str = "00000000-0000-4000-8000-000000000007";

    const FIXTURE_1: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000001.json");
    const FIXTURE_2: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000002.json");
    const FIXTURE_3: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000003.json");
    const FIXTURE_4: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000004.json");
    const FIXTURE_5: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000005.json");
    const FIXTURE_6: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000006.json");
    const FIXTURE_7: &[u8] =
        include_bytes!("../../../examples/sync/00000000-0000-4000-8000-000000000007.json");

    fn fixture(name: &str) -> &'static [u8] {
        match name {
            F1 => FIXTURE_1,
            F2 => FIXTURE_2,
            F3 => FIXTURE_3,
            F4 => FIXTURE_4,
            F5 => FIXTURE_5,
            F6 => FIXTURE_6,
            F7 => FIXTURE_7,
            _ => panic!("unknown fixture"),
        }
    }

    fn roundtrip(bytes: &[u8], basename: &str) -> (WireRecord, String) {
        let rec = parse(bytes, basename).expect("valid fixture must parse");
        let json = rec.to_json();
        let rec2 = parse(json.as_bytes(), basename).expect("serialized record must reparse");
        assert_eq!(rec, rec2, "roundtrip must be lossless");
        (rec2, json)
    }

    #[test]
    fn fresh_android_record_roundtrip() {
        let (rec, json) = roundtrip(fixture(F1), F1);
        assert_eq!(rec.v, 1);
        assert_eq!(rec.id, F1);
        assert_eq!(rec.created_at, 1713456000123);
        assert_eq!(rec.deleted_at, None);
        assert_eq!(
            rec.text,
            "Meeting notes: rename the module before the demo."
        );
        assert_eq!(rec.mode, "transcription");
        assert_eq!(rec.duration_ms, 8400);
        assert_eq!(rec.provider, "groq");
        assert_eq!(rec.model.as_deref(), Some("whisper-large-v3"));
        assert_eq!(rec.language.as_deref(), Some("en"));
        assert_eq!(
            json,
            r#"{"v":1,"id":"00000000-0000-4000-8000-000000000001","created_at":1713456000123,"deleted_at":null,"text":"Meeting notes: rename the module before the demo.","mode":"transcription","duration_ms":8400,"provider":"groq","model":"whisper-large-v3","language":"en"}"#
        );
    }

    #[test]
    fn minimal_windows_record_roundtrip() {
        let (rec, json) = roundtrip(fixture(F2), F2);
        assert_eq!(rec.duration_ms, 1200);
        assert_eq!(rec.provider, "openai");
        assert_eq!(rec.model, None);
        assert_eq!(rec.language, None);
        assert!(json.contains(r#""model":null"#));
        assert!(json.contains(r#""language":null"#));
    }

    #[test]
    fn tombstone_roundtrip() {
        let (rec, json) = roundtrip(fixture(F3), F3);
        assert_eq!(rec.deleted_at, Some(1713462000456));
        assert_eq!(rec.text, "");
        assert_eq!(rec.duration_ms, 0);
        assert_eq!(rec.provider, "");
        assert!(json.contains(r#""deleted_at":1713462000456"#));
    }

    #[test]
    fn agent_mode_roundtrip() {
        let (rec, _) = roundtrip(fixture(F4), F4);
        assert_eq!(rec.mode, "agent");
        assert_eq!(rec.model.as_deref(), Some("llama-3.3-70b-versatile"));
        assert_eq!(rec.created_at, 1713459000123);
    }

    #[test]
    fn malformed_json_rejected() {
        assert_eq!(
            parse(br#"{ not json"#, F1),
            Err(InvalidReason::MalformedJson)
        );
    }

    #[test]
    fn unknown_schema_version_rejected() {
        let (mut rec, _) = roundtrip(fixture(F1), F1);
        rec.v = 2;
        assert_eq!(
            parse(rec.to_json().as_bytes(), F1),
            Err(InvalidReason::UnknownSchemaVersion)
        );
    }

    #[test]
    fn filename_id_mismatch_rejected() {
        let bytes = fixture(F1);
        assert_eq!(parse(bytes, F2), Err(InvalidReason::IdNameMismatch));
    }

    #[test]
    fn negative_deleted_at_rejected() {
        let (mut rec, _) = roundtrip(fixture(F3), F3);
        rec.deleted_at = Some(-5);
        assert_eq!(
            parse(rec.to_json().as_bytes(), F3),
            Err(InvalidReason::BadTimestamp)
        );
    }

    #[test]
    fn null_model_roundtrips_as_null() {
        let (_, json) = roundtrip(fixture(F2), F2);
        assert!(json.contains(r#""model":null"#), "null must stay null");
    }

    #[test]
    fn empty_string_model_distinct_from_null() {
        let (mut rec, _) = roundtrip(fixture(F2), F2);
        assert_eq!(rec.model, None);
        rec.model = Some(String::new());
        let reparsed = parse(rec.to_json().as_bytes(), F2).expect("valid record");
        assert_eq!(reparsed.model, Some(String::new()));
        assert_ne!(reparsed.model, None, "empty string is distinct from null");
    }

    #[test]
    fn uuid_basename_accepts_valid_and_rejects_others() {
        assert_eq!(uuid_basename(&format!("{F1}.json")), Some(F1));
        assert_eq!(uuid_basename("X-copy.json"), None);
        assert_eq!(
            uuid_basename("00000000-0000-3000-8000-000000000001.json"),
            None
        );
        assert_eq!(
            uuid_basename("00000000-0000-4000-7000-000000000001.json"),
            None
        );
        assert_eq!(
            uuid_basename("00000000-0000-4000-8000-000000000001.txt"),
            None
        );
        assert_eq!(uuid_basename("00000000-0000-4000-8000-000000000001"), None);
    }

    fn dictionary_record(
        id: &str,
        spoken: &str,
        corrected: &str,
        kind: &str,
        deleted_at: Option<i64>,
    ) -> WireRecord {
        WireRecord {
            v: 1,
            id: id.to_string(),
            created_at: 1713465000123,
            deleted_at,
            rtype: RecordType::Dictionary,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            spoken: Some(spoken.to_string()),
            corrected: Some(corrected.to_string()),
            kind: Some(kind.to_string()),
            trigger: None,
            expansion: None,
            settings_key: None,
            settings_value: None,
        }
    }

    #[test]
    fn dictionary_record_roundtrip() {
        let (rec, json) = roundtrip(fixture(F5), F5);
        assert_eq!(rec.rtype, RecordType::Dictionary);
        assert_eq!(rec.spoken.as_deref(), Some("gonna"));
        assert_eq!(rec.corrected.as_deref(), Some("going to"));
        assert_eq!(rec.kind.as_deref(), Some("correction"));
        assert_eq!(rec.text, "");
        assert_eq!(rec.model, None);
        assert_eq!(
            rec.content(),
            RecordContent::Dictionary(DictionaryTuple {
                created_at: 1713465000123,
                spoken: "gonna".to_string(),
                corrected: "going to".to_string(),
                kind: "correction".to_string(),
            })
        );
        assert_eq!(
            json,
            String::from_utf8_lossy(FIXTURE_5).trim_end(),
            "dictionary record must serialize byte-identically to its fixture"
        );
    }

    #[test]
    fn snippet_record_roundtrip() {
        let (rec, json) = roundtrip(fixture(F6), F6);
        assert_eq!(rec.rtype, RecordType::Snippet);
        assert_eq!(rec.trigger.as_deref(), Some("addr"));
        assert_eq!(
            rec.expansion.as_deref(),
            Some("123 Example Street, Springfield")
        );
        assert_eq!(
            rec.content(),
            RecordContent::Snippet(SnippetTuple {
                created_at: 1713468000123,
                trigger: "addr".to_string(),
                expansion: "123 Example Street, Springfield".to_string(),
            })
        );
        assert_eq!(
            json,
            String::from_utf8_lossy(FIXTURE_6).trim_end(),
            "snippet record must serialize byte-identically to its fixture"
        );
    }

    #[test]
    fn settings_record_roundtrip() {
        let (rec, json) = roundtrip(fixture(F7), F7);
        assert_eq!(rec.rtype, RecordType::Settings);
        assert_eq!(rec.settings_key.as_deref(), Some("snippets_enabled"));
        assert_eq!(rec.settings_value.as_deref(), Some("true"));
        assert_eq!(
            rec.content(),
            RecordContent::Settings(SettingsTuple {
                created_at: 1713471000123,
                key: "snippets_enabled".to_string(),
                value: "true".to_string(),
            })
        );
        assert_eq!(
            json,
            String::from_utf8_lossy(FIXTURE_7).trim_end(),
            "settings record must serialize byte-identically to its fixture"
        );
    }

    #[test]
    fn unknown_type_rejected() {
        let rec = roundtrip(fixture(F1), F1).0;
        let json = rec
            .to_json()
            .replace(r#""deleted_at":null"#, r#""type":"note","deleted_at":null"#);
        assert_eq!(parse(json.as_bytes(), F1), Err(InvalidReason::UnknownType));
        let json = rec
            .to_json()
            .replace(r#""deleted_at":null"#, r#""type":5,"deleted_at":null"#);
        assert_eq!(parse(json.as_bytes(), F1), Err(InvalidReason::UnknownType));
    }

    #[test]
    fn missing_dictionary_fields_rejected() {
        let rec = dictionary_record(F5, "gonna", "going to", "correction", None);
        let mut v: serde_json::Value = serde_json::from_str(&rec.to_json()).expect("valid json");
        v.as_object_mut().expect("object").remove("spoken");
        assert_eq!(
            parse(v.to_string().as_bytes(), F5),
            Err(InvalidReason::MissingTypeField)
        );

        let blank = dictionary_record(F5, "gonna", "   ", "correction", None);
        assert_eq!(
            parse(blank.to_json().as_bytes(), F5),
            Err(InvalidReason::MissingTypeField)
        );

        let bad_kind = dictionary_record(F5, "gonna", "going to", "synonym", None);
        assert_eq!(
            parse(bad_kind.to_json().as_bytes(), F5),
            Err(InvalidReason::BadKind)
        );
    }

    #[test]
    fn missing_snippet_fields_rejected() {
        let mut rec = WireRecord {
            v: 1,
            id: F6.to_string(),
            created_at: 1713468000123,
            deleted_at: None,
            rtype: RecordType::Snippet,
            text: String::new(),
            mode: String::new(),
            duration_ms: 0,
            provider: String::new(),
            model: None,
            language: None,
            spoken: None,
            corrected: None,
            kind: None,
            trigger: Some("addr".to_string()),
            expansion: Some("123 Example Street, Springfield".to_string()),
            settings_key: None,
            settings_value: None,
        };
        rec.expansion = None;
        assert_eq!(
            parse(rec.to_json().as_bytes(), F6),
            Err(InvalidReason::MissingTypeField)
        );
    }

    #[test]
    fn missing_settings_key_rejected() {
        let mut v: serde_json::Value = serde_json::from_str(
            r#"{"v":1,"id":"00000000-0000-4000-8000-000000000007","created_at":1713471000123,"type":"settings","key":"snippets_enabled","value":"true"}"#,
        )
        .expect("valid json");
        v.as_object_mut().expect("object").remove("key");
        assert_eq!(
            parse(v.to_string().as_bytes(), F7),
            Err(InvalidReason::MissingTypeField)
        );
    }

    #[test]
    fn explicit_history_type_roundtrips_byte_identical() {
        let compact = roundtrip(fixture(F1), F1).1;
        let with_type = compact.replace(
            r#""deleted_at":null"#,
            r#""type":"history","deleted_at":null"#,
        );
        let rec = parse(with_type.as_bytes(), F1).expect("explicit history type must parse");
        assert_eq!(rec.rtype, RecordType::History);
        assert_eq!(
            rec.to_json(),
            compact,
            "history records must never serialize the type field"
        );
    }

    #[test]
    fn tombstoned_dictionary_record_parses() {
        let rec = dictionary_record(F5, "gonna", "going to", "correction", Some(1713469000123));
        let reparsed = parse(rec.to_json().as_bytes(), F5).expect("tombstoned dictionary parses");
        assert_eq!(reparsed.deleted_at, Some(1713469000123));
        assert_eq!(reparsed.rtype, RecordType::Dictionary);
        match reparsed.content() {
            RecordContent::Dictionary(t) => assert_eq!(t.spoken, "gonna"),
            other => panic!("expected dictionary content, got {other:?}"),
        }
    }

    #[test]
    fn kinds_are_never_content_equal() {
        let h = parse(fixture(F1), F1).expect("history").content();
        let d = parse(fixture(F5), F5).expect("dictionary").content();
        let s = parse(fixture(F6), F6).expect("snippet").content();
        let st = parse(fixture(F7), F7).expect("settings").content();
        assert!(!tuples_equal(&h, &d));
        assert!(!tuples_equal(&h, &s));
        assert!(!tuples_equal(&h, &st));
        assert!(!tuples_equal(&d, &s));
        assert!(!tuples_equal(&d, &st));
        assert!(!tuples_equal(&s, &st));
        assert!(tuples_equal(&d, &d));
    }
}
