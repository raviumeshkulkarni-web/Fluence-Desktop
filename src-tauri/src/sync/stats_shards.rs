// Fluence sync - Stats time shards (monthly per-event envelopes).
//
// The Stats ledger is one LOGICAL set of immutable per-dictation events
// (`StatsItem`, one row per completed dictation). It is stored as SEVERAL
// physical v1 envelopes so no single file can hit the 8 MiB / 50k caps:
//
//   appDataFolder/fluence/v1/stats.json                 <- legacy, FROZEN (never written by new clients)
//   appDataFolder/fluence/v1/stats-YYYY-MM.json          <- month shard, part 1
//   appDataFolder/fluence/v1/stats-YYYY-MM-pNN.json     <- month overflow parts (NN >= 02)
//
// Every shard is a COMPLETE v1 envelope: same `{"v":1,"entries":[...]}` shape,
// same `StatsItem` schema, same `(day, eventId)` sort, same per-record
// validation, same per-file 8 MiB / 50k caps, same union-dedup-by-eventId
// merge with the same LWW tiebreak, same CAS + upload-liveness per file.
// The architecture changes STORAGE LOCATION, never RECORD MEANING:
// sessions still equal record count, totals still equal field sums.
//
// Shard content rule (no migration, no rewrite of history):
// a shard holds exactly the merged rows of its month that are NOT
// byte-identical to the legacy `stats.json` version of the same eventId.
// Legacy rows are never copied, moved, or deleted; they are read as part of
// the union forever. Stale shard rows (losing LWW to a newer legacy edit by
// an old client) are dropped from later shard revisions by recomputation.
//
// Old released clients list `stats.json` by EXACT name, so shard siblings
// are invisible to them: they can neither read, overwrite, nor delete a
// shard. New clients union legacy + all shards on every read.
//
// Month derivation is UTC: a record belongs to the month of its `day`
// field, which producers stamp as the UTC calendar day of `timestamp_ms`.
// A record whose `day` is not a valid UTC date is UNROUTABLE (fail-closed):
// it is excluded from shard content, kept locally, and never uploaded.

use std::collections::{HashMap, HashSet};

use crate::sync::domain::{StatsEnvelope, StatsItem, ENVELOPE_V1, MAX_ENVELOPE_ITEMS};
use crate::sync::drive::MAX_DOMAIN_BYTES;
use crate::sync::error::SyncError;

/// Prefix/suffix framing every Stats shard file name. `stats.json` itself
/// (the frozen legacy file) never matches: it has no `-YYYY-MM` segment.
const SHARD_PREFIX: &str = "stats-";
const SHARD_SUFFIX: &str = ".json";
const SHARD_PART_SEP: &str = "-p";

/// First overflow part number. Part 1 is the base name (`stats-YYYY-MM.json`).
const FIRST_OVERFLOW_PART: u32 = 2;

/// Extract the UTC month (`YYYY-MM`) of a record `day` (`YYYY-MM-DD UTC).
/// Returns `None` for malformed days (fail-closed: unroutable, see above).
pub fn month_of_day(day: &str) -> Option<String> {
    if day.len() != 10 {
        return None;
    }
    // Strict date validation (leap days, month ranges) via the same parser
    // the record validator uses.
    if chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err() {
        return None;
    }
    Some(day[..7].to_string())
}

/// Canonical shard file for a UTC month: `stats-YYYY-MM.json`.
/// Returns `None` unless `month` is a well-formed `YYYY-MM` (01-12).
pub fn shard_file_for_month(month: &str) -> Option<String> {
    if !is_valid_month(month) {
        return None;
    }
    Some(format!("{SHARD_PREFIX}{month}{SHARD_SUFFIX}"))
}

/// Canonical file for chunk `part_index` (0-based) of a month: index 0 is the
/// base name, later chunks are `stats-YYYY-MM-pNN.json` (NN = part number).
/// Returns `None` for malformed months or part numbers above 999.
pub fn chunk_file_name(month: &str, part_index: usize) -> Option<String> {
    if !is_valid_month(month) {
        return None;
    }
    let part = (part_index as u32).checked_add(1)?;
    if part < FIRST_OVERFLOW_PART {
        debug_assert_eq!(part, 1, "part numbers start at 1");
        return Some(format!("{SHARD_PREFIX}{month}{SHARD_SUFFIX}"));
    }
    if part > 999 {
        return None;
    }
    if part < 100 {
        Some(format!(
            "{SHARD_PREFIX}{month}{SHARD_PART_SEP}{part:02}{SHARD_SUFFIX}"
        ))
    } else {
        Some(format!(
            "{SHARD_PREFIX}{month}{SHARD_PART_SEP}{part}{SHARD_SUFFIX}"
        ))
    }
}

fn is_valid_month(month: &str) -> bool {
    let b = month.as_bytes();
    if b.len() != 7 || b[4] != b'-' {
        return false;
    }
    if !b[..4].iter().all(|c| c.is_ascii_digit()) || !b[5..].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    matches!(month[5..].parse::<u32>(), Ok(m) if (1..=12).contains(&m))
}

/// True for Stats month/overflow shard names. Never true for the frozen
/// `stats.json`, other domains, backups, or case variants (Drive names are
/// case-sensitive; only exact lowercase shards are addressed).
pub fn is_stats_shard_file_name(name: &str) -> bool {
    shard_sort_key(name).is_some()
}

/// Sort key for a shard file: `(month, part)` with the base name as part 1.
/// Returns `None` for anything that is not a shard (see above).
pub fn shard_sort_key(name: &str) -> Option<(String, u32)> {
    let middle = name
        .strip_prefix(SHARD_PREFIX)?
        .strip_suffix(SHARD_SUFFIX)?;
    // Overflow form: `YYYY-MM-pNN`.
    if let Some((month, part)) = middle.split_once(SHARD_PART_SEP) {
        if !is_valid_month(month) {
            return None;
        }
        // Part numbers are 2+ ASCII digits on read (forward-tolerant).
        if part.len() < 2 || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u32 = part.parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some((month.to_string(), n));
    }
    // Base form: `YYYY-MM`.
    if !is_valid_month(middle) {
        return None;
    }
    Some((middle.to_string(), 1))
}

/// Canonical per-record ordering shared by every shard (matches the envelope
/// sort: day, then eventId). Chunking and desired-content computation sort by
/// this so packing is deterministic across devices and passes.
pub fn sort_stats_records(rows: &mut [StatsItem]) {
    rows.sort_by(|a, b| a.day.cmp(&b.day).then_with(|| a.event_id.cmp(&b.event_id)));
}

/// Serialized envelope bytes for a chunk candidate (single trailing newline
/// included, exactly as uploaded).
fn envelope_bytes(rows: &[StatsItem]) -> Vec<u8> {
    StatsEnvelope {
        v: ENVELOPE_V1,
        entries: rows.to_vec(),
    }
    .to_bytes()
}

/// True when `rows` fit one physical file: at most 50k records AND at most
/// 8 MiB serialized. A single valid record always fits (bounded schema).
fn chunk_fits(rows: &[StatsItem]) -> bool {
    if rows.len() > MAX_ENVELOPE_ITEMS {
        return false;
    }
    envelope_bytes(rows).len() <= MAX_DOMAIN_BYTES
}

/// Split an oversize slice into fitting halves, recursively. Each half is
/// strictly smaller and singletons always fit, so this terminates.
fn split_oversize(sorted: Vec<StatsItem>) -> Vec<Vec<StatsItem>> {
    if chunk_fits(&sorted) {
        return vec![sorted];
    }
    // Binary split (first half keeps the extra row on odd counts).
    let mid = sorted.len().div_ceil(2);
    let mut rest = sorted;
    let tail = rest.split_off(mid);
    let mut out = split_oversize(rest);
    out.extend(split_oversize(tail));
    out
}

/// Split a month's sorted rows into deterministic chunks, each fitting one
/// file. Greedy fill by count first (stable: appending newer rows only grows
/// the LAST chunk), then byte-split only the chunks that exceed 8 MiB.
/// The concatenation of all chunks in order always equals the sorted input.
fn split_into_chunks(sorted: Vec<StatsItem>) -> Vec<Vec<StatsItem>> {
    let mut out = Vec::new();
    // `chunks` yields full 50k slices then the remainder; empty input yields
    // none (handled by the caller, but harmless here too).
    for count_chunk in sorted.chunks(MAX_ENVELOPE_ITEMS) {
        out.extend(split_oversize(count_chunk.to_vec()));
    }
    out
}

/// Pack one month's rows into named chunk files. Returns `(file_name, rows)`
/// in part order. Empty input yields no chunks (no empty file is created).
/// The concatenation of all chunk rows in order equals the sorted input.
///
/// Stability tradeoff (deliberate): greedy fill by count keeps appends
/// (newer days sort last) confined to the LAST chunk, so steady state
/// rewrites one small file per pass. Crossing 50k rows renumbers later
/// `-pNN` parts once, and byte-splitting an oversize chunk bisects it —
/// both rewrite a few files in that pass. Always correct (content
/// converges, CAS protects, nothing is lost), just briefly more PUTs.
/// Count-only chunking was rejected: 50k worst-case rows exceed 8 MiB, so
/// the byte check is what keeps every chunk uploadable.
/// Name pre-split chunks for a month in part order. Fails with an explicit
/// [`SyncError::Fatal`] when the month needs more than the 999 addressable
/// parts (`chunk_file_name` returns `None` past part 999) instead of panicking
/// mid-pass. Same severity as the old panic path (which `catch_unwind`
/// converted to Fatal and parked scheduling), but honest and greppable, with
/// the local ledger left intact so nothing is uploaded partially. Only
/// reachable with absurd inputs (1000 chunks need tens of millions of rows
/// in one month); ordinary months never see this.
pub fn assign_chunk_names(
    month: &str,
    chunks: Vec<Vec<StatsItem>>,
) -> Result<Vec<(String, Vec<StatsItem>)>, SyncError> {
    let mut out = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.into_iter().enumerate() {
        match chunk_file_name(month, i) {
            Some(name) => out.push((name, chunk)),
            None => {
                return Err(SyncError::Fatal(format!(
                    "stats month {month} needs more than 999 shard parts; ledger retained locally, nothing uploaded"
                )));
            }
        }
    }
    Ok(out)
}

pub fn chunk_month_records(
    month: &str,
    rows: Vec<StatsItem>,
) -> Result<Vec<(String, Vec<StatsItem>)>, SyncError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut sorted = rows;
    sort_stats_records(&mut sorted);
    assign_chunk_names(month, split_into_chunks(sorted))
}

/// N-way union merge over ordered record lists. The merge is a fold over
/// [`crate::sync::merge::merge_stats`], which is a TOTAL order (H2): same
/// updatedAt AND same deviceId but different payload resolves by
/// (day, timestamp_ms, words, chars, duration_ms), so the outcome depends
/// only on the record set, not on list order. Truly identical rows still tie,
/// and then fold order decides — callers pass `[local, legacy, shards...]` so
/// local wins those, preserving the two-list upload tie-guard exactly.
pub fn union_stats_merge(ordered_lists: &[Vec<StatsItem>]) -> Vec<StatsItem> {
    let mut acc: Vec<StatsItem> = Vec::new();
    for list in ordered_lists {
        let outcome = crate::sync::merge::merge_stats(&acc, list);
        acc = outcome.merged;
    }
    acc
}

/// Desired shard rows for one month: merged rows of `month` MINUS rows that
/// are byte-identical (full `StatsItem` equality) to the legacy version of
/// the same eventId. New rows and LWW-updated rows are kept; history already
/// present in `stats.json` is never recopied.
pub fn desired_shard_rows(
    month: &str,
    merged_sorted: &[StatsItem],
    legacy_by_id: &HashMap<String, StatsItem>,
) -> Vec<StatsItem> {
    merged_sorted
        .iter()
        .filter(|r| {
            if month_of_day(&r.day).as_deref() != Some(month) {
                return false;
            }
            match legacy_by_id.get(&r.event_id) {
                Some(legacy) => legacy != *r,
                None => true,
            }
        })
        .cloned()
        .collect()
}

/// Index helper: eventId -> record for the legacy file's entries.
pub fn index_by_event_id(rows: &[StatsItem]) -> HashMap<String, StatsItem> {
    rows.iter()
        .map(|r| (r.event_id.clone(), r.clone()))
        .collect()
}

/// Months present in a record list (valid UTC months only), sorted.
pub fn months_in(rows: &[StatsItem]) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();
    for r in rows {
        if let Some(m) = month_of_day(&r.day) {
            set.insert(m);
        }
    }
    let mut out: Vec<String> = set.into_iter().collect();
    out.sort();
    out
}

/// First overflow part number (public for engine/tests).
pub fn first_overflow_part() -> u32 {
    FIRST_OVERFLOW_PART
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(event_id: &str, day: &str, words: i64) -> StatsItem {
        StatsItem {
            event_id: event_id.to_string(),
            day: day.to_string(),
            timestamp_ms: 1_700_000_000_000,
            words: Some(words),
            chars: Some(words * 5),
            duration_ms: Some(1_000),
            updated_at: None,
            device_id: None,
        }
    }

    fn uuid(n: u8) -> String {
        format!("123e4567-e89b-12d3-a456-4266141740{n:02}")
    }

    // ── Naming vectors (cross-platform contract: Android asserts the same) ──

    #[test]
    fn month_of_day_vectors() {
        assert_eq!(month_of_day("2026-08-20").as_deref(), Some("2026-08"));
        assert_eq!(month_of_day("2024-02-29").as_deref(), Some("2024-02")); // leap day
        assert_eq!(month_of_day("2025-12-31").as_deref(), Some("2025-12")); // year edge
        assert_eq!(month_of_day("2026-01-01").as_deref(), Some("2026-01")); // year edge
        assert_eq!(month_of_day("1970-01-01").as_deref(), Some("1970-01"));
        // Malformed -> unroutable.
        assert_eq!(month_of_day(""), None);
        assert_eq!(month_of_day("2026-8-5"), None);
        assert_eq!(month_of_day("2026-13-01"), None);
        assert_eq!(month_of_day("2026-00-10"), None);
        assert_eq!(month_of_day("2023-02-29"), None); // not a leap year
        assert_eq!(month_of_day("not-a-day!"), None);
        assert_eq!(month_of_day("2026-08-20T00:00:00"), None);
    }

    #[test]
    fn shard_file_for_month_vectors() {
        assert_eq!(
            shard_file_for_month("2026-08").as_deref(),
            Some("stats-2026-08.json")
        );
        assert_eq!(
            shard_file_for_month("2024-02").as_deref(),
            Some("stats-2024-02.json")
        );
        assert_eq!(shard_file_for_month("2026-13"), None);
        assert_eq!(shard_file_for_month("2026-00"), None);
        assert_eq!(shard_file_for_month("26-08"), None);
        assert_eq!(shard_file_for_month(""), None);
    }

    #[test]
    fn chunk_file_names_overflow() {
        assert_eq!(first_overflow_part(), 2);
        assert_eq!(
            chunk_file_name("2026-08", 0).as_deref(),
            Some("stats-2026-08.json")
        );
        assert_eq!(
            chunk_file_name("2026-08", 1).as_deref(),
            Some("stats-2026-08-p02.json")
        );
        assert_eq!(
            chunk_file_name("2026-08", 2).as_deref(),
            Some("stats-2026-08-p03.json")
        );
        assert_eq!(
            chunk_file_name("2026-08", 98).as_deref(),
            Some("stats-2026-08-p99.json")
        );
        assert_eq!(chunk_file_name("2026-13", 0), None);
    }

    #[test]
    fn shard_name_accept_reject() {
        // Accept.
        assert!(is_stats_shard_file_name("stats-2026-08.json"));
        assert!(is_stats_shard_file_name("stats-2024-02.json"));
        assert!(is_stats_shard_file_name("stats-2026-08-p02.json"));
        assert!(is_stats_shard_file_name("stats-2026-08-p99.json"));
        // Reject: legacy, other domains, backups, bad shapes, case variants.
        assert!(!is_stats_shard_file_name("stats.json"));
        assert!(!is_stats_shard_file_name("dictionary.json"));
        assert!(!is_stats_shard_file_name("snippets.json"));
        assert!(!is_stats_shard_file_name("settings.json"));
        assert!(!is_stats_shard_file_name("stats.json.bak"));
        assert!(!is_stats_shard_file_name("stats-2026-8.json"));
        assert!(!is_stats_shard_file_name("stats-2026-13.json"));
        assert!(!is_stats_shard_file_name("stats-2026-00.json"));
        assert!(!is_stats_shard_file_name("stats-26-08.json"));
        assert!(!is_stats_shard_file_name("STATS-2026-08.json"));
        assert!(!is_stats_shard_file_name("stats-2026-08-p1.json"));
        assert!(!is_stats_shard_file_name("stats-2026-08-p00.json"));
        assert!(!is_stats_shard_file_name("stats-2026-08-p02.json.bak"));
        assert!(!is_stats_shard_file_name("agents.json"));
    }

    #[test]
    fn shard_sort_keys_order_parts() {
        assert_eq!(
            shard_sort_key("stats-2026-08.json"),
            Some(("2026-08".to_string(), 1))
        );
        assert_eq!(
            shard_sort_key("stats-2026-08-p02.json"),
            Some(("2026-08".to_string(), 2))
        );
        assert_eq!(shard_sort_key("stats.json"), None);
        let mut names = vec![
            "stats-2026-09.json",
            "stats-2026-08-p02.json",
            "stats-2026-08.json",
        ];
        names.sort_by_key(|n| shard_sort_key(n));
        assert_eq!(
            names,
            vec![
                "stats-2026-08.json",
                "stats-2026-08-p02.json",
                "stats-2026-09.json",
            ]
        );
    }

    // ── Chunking ──

    /// F4: every produced chunk must satisfy the upload roundtrip gate
    /// (`StatsEnvelope::from_bytes(bytes).entries == chunk`) through the real
    /// parser — including multi-chunk splits and rows with absent optionals.
    /// Characterization (green by construction today): pins the gate's
    /// predicate against future schema drift, which is the only way the gate
    /// in `sync_stats_sharded` can ever fire.
    #[test]
    fn every_chunk_round_trips_through_the_real_parser() {
        let mut rows = vec![
            item(&uuid(1), "2026-08-02", 7),
            item(&uuid(2), "2026-08-20", 3),
        ];
        rows.push(StatsItem {
            event_id: uuid(3),
            day: "2026-08-03".to_string(),
            timestamp_ms: 1_700_000_000_000,
            words: None,
            chars: None,
            duration_ms: None,
            updated_at: Some(1_700_000_000_001),
            device_id: Some("dev-a".to_string()),
        });
        let chunks = chunk_month_records("2026-08", rows).expect("3 rows fit the part cap");
        assert_eq!(chunks.len(), 1);
        for (name, entries) in &chunks {
            let bytes = envelope_bytes(entries);
            let parsed = StatsEnvelope::from_bytes(&bytes)
                .unwrap_or_else(|| panic!("chunk {name} must re-parse"));
            assert_eq!(
                parsed.entries, *entries,
                "chunk {name} must round-trip byte-exact"
            );
        }
    }

    #[test]
    fn empty_month_yields_no_chunks() {
        assert!(chunk_month_records("2026-08", vec![])
            .expect("empty input cannot exceed the part cap")
            .is_empty());
    }

    /// F4: more than 999 chunks for one month must be an explicit error, never
    /// a mid-pass panic. (Only reachable synthetically: 1000 chunks need tens
    /// of millions of rows. The naming helper is tested directly with
    /// pre-split chunks instead of materializing that input.)
    #[test]
    fn over_999_chunks_is_an_explicit_error_not_a_panic() {
        let chunks: Vec<Vec<StatsItem>> = (0..1001).map(|_| Vec::new()).collect();
        match assign_chunk_names("2026-08", chunks) {
            Err(crate::sync::error::SyncError::Fatal(msg)) => assert!(
                msg.contains("999"),
                "error must name the part-number ceiling, got: {msg}"
            ),
            Err(other) => panic!("chunk overflow must be Fatal, got: {other:?}"),
            Ok(_) => panic!("1001 chunks must not name successfully"),
        }
    }

    #[test]
    fn small_month_is_one_base_chunk_sorted() {
        let rows = vec![
            item(&uuid(3), "2026-08-20", 3),
            item(&uuid(1), "2026-08-02", 1),
            item(&uuid(2), "2026-08-02", 2),
        ];
        let chunks =
            chunk_month_records("2026-08", rows).expect("3 rows cannot exceed the part cap");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "stats-2026-08.json");
        let ids: Vec<&str> = chunks[0].1.iter().map(|r| r.event_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![uuid(1).as_str(), uuid(2).as_str(), uuid(3).as_str()]
        );
        // Envelope round-trips through the real parser.
        let bytes = envelope_bytes(&chunks[0].1);
        let parsed = StatsEnvelope::from_bytes(&bytes).expect("chunk must parse");
        assert_eq!(parsed.entries.len(), 3);
        assert!(bytes.ends_with(b"\n"));
    }

    #[test]
    fn chunking_is_deterministic_regardless_of_input_order() {
        let mk = || {
            (0u8..50)
                .map(|i| item(&uuid(i), &format!("2026-08-{:02}", (i % 28) + 1), i as i64))
                .collect::<Vec<_>>()
        };
        let a = mk();
        let mut b = mk();
        b.reverse();
        // uuid() only yields 100 distinct ids; fine for determinism check.
        let ca = chunk_month_records("2026-08", a).expect("50 rows fit the part cap");
        let cb = chunk_month_records("2026-08", b).expect("50 rows fit the part cap");
        assert_eq!(ca, cb);
    }

    #[test]
    fn count_overflow_spills_into_p02() {
        // 50_001 minimal rows -> base (50k) + p02 (1). Rows stay tiny so the
        // byte cap never binds; this exercises the count cap path only.
        let row_at = |i: usize| StatsItem {
            event_id: format!(
                "{:08x}-0000-4000-8000-{:012x}",
                i,
                (i as u64).wrapping_mul(2654435761) & 0xffffffffffff
            ),
            day: "2026-08-15".to_string(),
            timestamp_ms: 1_700_000_000_000,
            words: Some(1),
            chars: Some(5),
            duration_ms: Some(100),
            updated_at: None,
            device_id: None,
        };
        let rows: Vec<StatsItem> = (0..MAX_ENVELOPE_ITEMS + 1).map(row_at).collect();
        // Sanity: ids unique and valid.
        let mut seen = HashSet::new();
        for r in &rows {
            assert!(r.validate(), "generated row must validate");
            assert!(seen.insert(r.event_id.clone()), "ids must be unique");
        }
        let chunks = chunk_month_records("2026-08", rows).expect("2 chunks fit the part cap");
        assert_eq!(chunks.len(), 2, "50_001 rows must spill into p02");
        assert_eq!(chunks[0].0, "stats-2026-08.json");
        assert_eq!(chunks[1].0, "stats-2026-08-p02.json");
        assert_eq!(chunks[0].1.len(), MAX_ENVELOPE_ITEMS);
        assert_eq!(chunks[1].1.len(), 1);
        // Every chunk parses and fits both caps.
        for (name, entries) in &chunks {
            assert!(entries.len() <= MAX_ENVELOPE_ITEMS, "{name} count cap");
            let bytes = envelope_bytes(entries);
            assert!(bytes.len() <= MAX_DOMAIN_BYTES, "{name} byte cap");
            let parsed = StatsEnvelope::from_bytes(&bytes).expect("chunk must parse");
            assert_eq!(parsed.entries.len(), entries.len());
        }
        // Concatenation in order equals the canonical sorted input.
        let all: Vec<StatsItem> = chunks.into_iter().flat_map(|(_, e)| e).collect();
        let mut expect: Vec<StatsItem> = (0..MAX_ENVELOPE_ITEMS + 1).map(row_at).collect();
        sort_stats_records(&mut expect);
        assert_eq!(all, expect);
    }

    // ── Union + diff ──

    #[test]
    fn union_merge_dedups_and_sums_inputs() {
        let a = item(&uuid(1), "2026-08-01", 10);
        let b = item(&uuid(2), "2026-09-01", 20);
        let mut b_newer = b.clone();
        b_newer.words = Some(25);
        b_newer.updated_at = Some(999);
        b_newer.device_id = Some("d".to_string());
        // Order [local, legacy, shard]: duplicates collapse, LWW applies.
        let merged = union_stats_merge(&[vec![b_newer.clone()], vec![a.clone()], vec![b.clone()]]);
        assert_eq!(merged.len(), 2);
        let got: HashMap<_, _> = merged
            .into_iter()
            .map(|r| (r.event_id.clone(), r))
            .collect();
        assert_eq!(got[&uuid(1)], a);
        assert_eq!(got[&uuid(2)], b_newer);
        // Sessions preserved: 2 records.
        assert_eq!(got.len(), 2);
        // Totals identical to the single-ledger baseline.
        let words: i64 = got.values().map(|r| r.words.unwrap_or(0)).sum();
        assert_eq!(words, 10 + 25);
    }

    #[test]
    fn desired_shard_rows_excludes_legacy_identical() {
        let legacy_row = item(&uuid(1), "2026-08-01", 10);
        let new_row = item(&uuid(2), "2026-08-02", 20);
        // Merged (deduped) input: the uuid(1) winner differs from legacy, so
        // it must be kept; the legacy-identical copy is dropped.
        let mut updated = legacy_row.clone();
        updated.words = Some(11);
        updated.updated_at = Some(500);
        let merged = vec![updated.clone(), new_row.clone()];
        let legacy_by_id = index_by_event_id(&[legacy_row.clone()]);
        let desired = desired_shard_rows("2026-08", &merged, &legacy_by_id);
        assert_eq!(desired, vec![updated, new_row]);
        // Fully legacy-identical input yields no shard rows (no rewrite).
        let merged2 = vec![legacy_row.clone()];
        let desired2 = desired_shard_rows("2026-08", &merged2, &legacy_by_id);
        assert!(
            desired2.is_empty(),
            "history in stats.json is never recopied"
        );
    }

    #[test]
    fn desired_shard_rows_keeps_lww_updated_legacy_row() {
        // Legacy holds an older version; merged winner is newer -> shard keeps it.
        let mut legacy_row = item(&uuid(1), "2026-08-01", 10);
        legacy_row.updated_at = Some(100);
        legacy_row.device_id = Some("a".to_string());
        let mut newer = legacy_row.clone();
        newer.words = Some(12);
        newer.updated_at = Some(200);
        let merged = union_stats_merge(&[vec![newer.clone()], vec![legacy_row.clone()]]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].words, Some(12));
        let legacy_by_id = index_by_event_id(&[legacy_row]);
        let desired = desired_shard_rows("2026-08", &merged, &legacy_by_id);
        assert_eq!(desired, vec![newer]);
    }

    #[test]
    fn desired_shard_rows_scoped_to_month() {
        let aug = item(&uuid(1), "2026-08-01", 10);
        let sep = item(&uuid(2), "2026-09-01", 20);
        let merged = vec![aug.clone(), sep.clone()];
        let legacy_by_id = HashMap::new();
        assert_eq!(
            desired_shard_rows("2026-08", &merged, &legacy_by_id),
            vec![aug]
        );
        assert_eq!(
            desired_shard_rows("2026-09", &merged, &legacy_by_id),
            vec![sep]
        );
        assert!(desired_shard_rows("2026-10", &merged, &legacy_by_id).is_empty());
    }

    #[test]
    fn months_in_lists_valid_months_sorted() {
        let rows = vec![
            item(&uuid(2), "2026-09-01", 1),
            item(&uuid(1), "2026-08-01", 1),
            item(&uuid(3), "2026-08-15", 1),
        ];
        assert_eq!(
            months_in(&rows),
            vec!["2026-08".to_string(), "2026-09".to_string()]
        );
    }

    #[test]
    fn legacy_file_name_is_never_a_shard() {
        assert!(!is_stats_shard_file_name(crate::sync::drive::STATS_FILE));
        assert_eq!(crate::sync::drive::STATS_FILE, "stats.json");
    }
}
