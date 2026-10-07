// Fluence sync - frozen v1.2 merges (dictionary, snippet, settings, stats)
//
// Winner: max(updatedAt, deviceId) - pure LWW. A tombstone is just another
// state transition: it wins when it is the newest record for a business key,
// and loses to any newer edit or re-creation. Tombstones are kept forever
// (never GC'd; the dataset is tiny).
//
// Dictionary/snippet identity is the business key:
//   dict    = lower(trim(spoken))
//   snippet = lower(trim(trigger))
// so two devices creating the same word offline converge to one winner.
//
// Settings: per-key LWW over the frozen five allowed keys.
// Stats: union dedup by eventId - summation happens at display time, so the
// same event can never be counted twice.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::sync::clock::cmp_keyed_winner;
use crate::sync::clock::cmp_winner;
use crate::sync::domain::*;

#[derive(Debug, Clone)]
pub struct MergeOutcome<T> {
    pub merged: Vec<T>,
    pub changed: bool,
}

/// Merge dictionary items: one winner per businessKey using pure LWW.
///
/// The comparison is a *total* order (`cmp_keyed_winner` falls back to
/// `sync_id`), so the winner never depends on whether a record arrived as the
/// local or the remote copy. Android's `Merge` applies the identical rule.
fn merge_keyed<T, K>(
    local: &[T],
    remote: &[T],
    business_key: impl Fn(&T) -> K,
    mut updated_at: impl FnMut(&T) -> i64,
    mut device_id: impl FnMut(&T) -> &str,
    mut sync_id: impl FnMut(&T) -> &str,
    mut sort_by: impl FnMut(&mut [T]),
) -> MergeOutcome<T>
where
    T: Clone + PartialEq,
    K: Ord + Clone + Eq + std::hash::Hash,
{
    let mut grouped: BTreeMap<K, Vec<T>> = BTreeMap::new();
    for item in local.iter().chain(remote.iter()) {
        grouped
            .entry(business_key(item))
            .or_default()
            .push(item.clone());
    }
    let local_map: HashMap<K, T> = local.iter().map(|i| (business_key(i), i.clone())).collect();

    let mut merged = Vec::new();
    let mut changed = false;
    for (bk, candidates) in grouped {
        let winner = candidates
            .into_iter()
            .max_by(|a, b| {
                cmp_keyed_winner(
                    updated_at(a),
                    device_id(a),
                    sync_id(a),
                    updated_at(b),
                    device_id(b),
                    sync_id(b),
                )
            })
            .unwrap();
        match local_map.get(&bk) {
            Some(local_item) if local_item == &winner => {}
            _ => changed = true,
        }
        merged.push(winner);
    }
    sort_by(&mut merged);
    MergeOutcome { merged, changed }
}

/// Merge dictionary items: one winner per businessKey using pure LWW.
pub fn merge_dictionary(
    local: &[DictionaryItem],
    remote: &[DictionaryItem],
) -> MergeOutcome<DictionaryItem> {
    merge_keyed(
        local,
        remote,
        |i| i.business_key(),
        |i| i.updated_at,
        |i| i.device_id.as_str(),
        |i| i.sync_id.as_str(),
        |v| v.sort_by(|a, b| a.business_key().cmp(&b.business_key())),
    )
}

/// Merge snippets: one winner per businessKey using pure LWW.
pub fn merge_snippets(local: &[SnippetItem], remote: &[SnippetItem]) -> MergeOutcome<SnippetItem> {
    merge_keyed(
        local,
        remote,
        |i| i.business_key(),
        |i| i.updated_at,
        |i| i.device_id.as_str(),
        |i| i.sync_id.as_str(),
        |v| v.sort_by(|a, b| a.business_key().cmp(&b.business_key())),
    )
}

/// Merge agents: one winner per businessKey using pure LWW.
///
/// The business key is the record's own stable id (`agent:<uuid>`), validated
/// at ingest but never recomputed — unlike dictionary/snippets, where the key
/// derives from content. The sort is (businessKey, syncId), matching Android's
/// canonical order byte-for-byte, so both sides serialize identical envelopes
/// for identical winner sets.
pub fn merge_agents(local: &[AgentItem], remote: &[AgentItem]) -> MergeOutcome<AgentItem> {
    merge_keyed(
        local,
        remote,
        |i| i.business_key.clone(),
        |i| i.updated_at,
        |i| i.device_id.as_str(),
        |i| i.sync_id.as_str(),
        |v| {
            v.sort_by(|a, b| {
                a.business_key
                    .cmp(&b.business_key)
                    .then_with(|| a.sync_id.cmp(&b.sync_id))
            })
        },
    )
}

/// Merge styles: identical contract to [`merge_agents`] with the
/// `custom:<uuid>` keyspace.
pub fn merge_styles(local: &[StyleItem], remote: &[StyleItem]) -> MergeOutcome<StyleItem> {
    merge_keyed(
        local,
        remote,
        |i| i.business_key.clone(),
        |i| i.updated_at,
        |i| i.device_id.as_str(),
        |i| i.sync_id.as_str(),
        |v| {
            v.sort_by(|a, b| {
                a.business_key
                    .cmp(&b.business_key)
                    .then_with(|| a.sync_id.cmp(&b.sync_id))
            })
        },
    )
}

/// Settings adoption sentinel - byte-identical to Android's `Merge.kt`
/// (1700000000000L). Both platforms stamp a first-observed settings key with
/// this same fixed epoch, so two simultaneous first observers tie-break by
/// deviceId instead of racing wall clocks, and the shared envelope carries
/// timestamps both sides interpret identically.
const SETTINGS_ADOPTION_EPOCH_MS: i64 = 1_700_000_000_000;

/// The adoption sentinel is a first-observation marker written by
/// `merge_settings` itself, not a real Nov-2023 timestamp. Map it back to the
/// adoption class on the read/compare path, mirroring Android's
/// `Merge.effectiveSettingsTime`. The stored record keeps its original
/// timestamp; only the comparison uses the effective value. The output stamp
/// further below is deliberately unchanged.
fn effective_settings_time(updated_at: i64) -> i64 {
    if updated_at == SETTINGS_ADOPTION_EPOCH_MS {
        0
    } else {
        updated_at
    }
}

/// Settings merge: per-key LWW over the allowed keys only.
pub fn merge_settings(
    local: &[SettingsItem],
    remote: &[SettingsItem],
) -> MergeOutcome<SettingsItem> {
    let mut map: HashMap<String, SettingsItem> = HashMap::new();
    for item in local.iter().chain(remote.iter()) {
        if !is_allowed_settings_key(&item.key) {
            continue;
        }
        match map.entry(item.key.clone()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(item.clone());
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let existing = e.get();
                let existing_at = effective_settings_time(existing.updated_at);
                let item_at = effective_settings_time(item.updated_at);
                let winner_is_item = if existing_at == 0 && item_at == 0 {
                    cmp_winner(item_at, &item.device_id, existing_at, &existing.device_id)
                        == std::cmp::Ordering::Greater
                } else if existing_at == 0 {
                    true
                } else if item_at == 0 {
                    false
                } else {
                    cmp_winner(item_at, &item.device_id, existing_at, &existing.device_id)
                        == std::cmp::Ordering::Greater
                };
                if winner_is_item {
                    e.insert(item.clone());
                }
            }
        }
    }
    for v in map.values_mut() {
        if v.updated_at == 0 {
            v.updated_at = SETTINGS_ADOPTION_EPOCH_MS;
        }
    }
    let mut merged: Vec<SettingsItem> = map.into_values().collect();
    merged.sort_by(|a, b| a.key.cmp(&b.key));
    let changed = {
        let local_set: HashSet<String> = local
            .iter()
            .filter(|i| is_allowed_settings_key(&i.key))
            .map(|i| format!("{}|{}|{}", i.key, i.value, i.updated_at))
            .collect();
        let merged_set: HashSet<String> = merged
            .iter()
            .map(|i| format!("{}|{}|{}", i.key, i.value, i.updated_at))
            .collect();
        local_set != merged_set
    };
    MergeOutcome { merged, changed }
}

/// Stats merge: union dedup by eventId. Normally duplicate event ids carry the
/// same immutable fact; if damaged/old data disagrees, use the same deterministic
/// LWW rule as Android so both platforms converge to one record.
pub fn merge_stats(local: &[StatsItem], remote: &[StatsItem]) -> MergeOutcome<StatsItem> {
    let mut map: HashMap<String, StatsItem> = HashMap::new();
    for item in local.iter().chain(remote.iter()) {
        match map.entry(item.event_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(item.clone());
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get();
                let existing_time = existing.updated_at.unwrap_or(0);
                let item_time = item.updated_at.unwrap_or(0);
                if cmp_winner(
                    item_time,
                    item.device_id.as_deref().unwrap_or(""),
                    existing_time,
                    existing.device_id.as_deref().unwrap_or(""),
                ) == std::cmp::Ordering::Greater
                {
                    entry.insert(item.clone());
                }
            }
        }
    }
    let mut merged: Vec<StatsItem> = map.into_values().collect();
    merged.sort_by(|a, b| a.day.cmp(&b.day).then_with(|| a.event_id.cmp(&b.event_id)));
    let changed = {
        let local_records: HashSet<StatsItem> = local.iter().cloned().collect();
        let merged_records: HashSet<StatsItem> = merged.iter().cloned().collect();
        local_records != merged_records
    };
    MergeOutcome { merged, changed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(
        spoken: &str,
        corrected: &str,
        kind: &str,
        deleted: bool,
        updated_at: i64,
        device_id: &str,
    ) -> DictionaryItem {
        DictionaryItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            spoken: spoken.to_string(),
            corrected: corrected.to_string(),
            kind: kind.to_string(),
            is_enabled: true,
            deleted_at: if deleted { Some(updated_at) } else { None },
            updated_at,
            device_id: device_id.to_string(),
        }
    }

    fn snippet(
        trigger: &str,
        expansion: &str,
        deleted: bool,
        updated_at: i64,
        device_id: &str,
    ) -> SnippetItem {
        SnippetItem {
            sync_id: uuid::Uuid::new_v4().to_string(),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            is_enabled: true,
            deleted_at: if deleted { Some(updated_at) } else { None },
            updated_at,
            device_id: device_id.to_string(),
        }
    }

    // ── Pure-LWW delete/re-creation semantics ───────────────────────────

    #[test]
    fn newer_delete_beats_older_live() {
        let live = dict("hello", "hi", "correction", false, 100, "a");
        let tomb = dict("hello", "hi", "correction", true, 200, "b");
        let outcome = merge_dictionary(&[live], &[tomb]);
        assert!(outcome.merged[0].deleted_at.is_some(), "newer delete wins");
    }

    #[test]
    fn newer_live_beats_older_tombstone_recreation_works() {
        // User deletes "hello" at t=100, re-adds it at t=200 → live again.
        let tomb = dict("hello", "hi", "correction", true, 100, "a");
        let recreated = dict("hello", "hi", "correction", false, 200, "b");
        let outcome = merge_dictionary(&[recreated], &[tomb]);
        assert!(
            outcome.merged[0].deleted_at.is_none(),
            "newer re-creation must beat older tombstone"
        );
    }

    #[test]
    fn older_live_never_resurrects_over_newer_tombstone() {
        // Offline device with an old edit must not resurrect a newer deletion.
        let stale_edit = dict("hello", "hi-old", "correction", false, 50, "a");
        let tomb = dict("hello", "hi", "correction", true, 200, "b");
        let outcome = merge_dictionary(&[stale_edit], &[tomb]);
        assert!(outcome.merged[0].deleted_at.is_some());
    }

    #[test]
    fn delete_vs_concurrent_edit_is_deterministic_regardless_of_order() {
        let edit = dict("hello", "edited", "correction", false, 150, "a");
        let tomb = dict("hello", "hi", "correction", true, 150, "z");
        let ab = merge_dictionary(&[edit.clone()], &[tomb.clone()]);
        let ba = merge_dictionary(&[tomb], &[edit]);
        assert_eq!(
            ab.merged[0].deleted_at.is_some(),
            ba.merged[0].deleted_at.is_some()
        );
        assert_eq!(ab.merged[0].device_id, ba.merged[0].device_id);
    }

    #[test]
    fn simultaneous_timestamps_use_device_id_tiebreak() {
        let a = dict("hello", "from-a", "correction", false, 100, "a");
        let b = dict("hello", "from-b", "correction", false, 100, "b");
        let outcome = merge_dictionary(&[a], &[b.clone()]);
        assert_eq!(outcome.merged[0].device_id, "b");
        let outcome2 = merge_dictionary(
            &[b],
            &[dict("hello", "from-a", "correction", false, 100, "a")],
        );
        assert_eq!(outcome2.merged[0].device_id, "b");
    }

    #[test]
    fn offline_edits_converge_regardless_of_merge_order() {
        // Three devices, three concurrent edits, all orderings agree.
        let e1 = dict("w", "one", "correction", false, 100, "dev-1");
        let e2 = dict("w", "two", "correction", false, 200, "dev-2");
        let e3 = dict("w", "three", "correction", false, 300, "dev-3");
        let r1 = merge_dictionary(
            &merge_dictionary(&[e1.clone()], &[e2.clone()]).merged,
            &[e3.clone()],
        );
        let r2 = merge_dictionary(
            &merge_dictionary(&[e3.clone()], &[e1.clone()]).merged,
            &[e2.clone()],
        );
        let r3 = merge_dictionary(
            &merge_dictionary(&[e2.clone()], &[e3.clone()]).merged,
            &[e1.clone()],
        );
        assert_eq!(r1.merged[0].corrected, "three");
        assert_eq!(r2.merged[0].corrected, "three");
        assert_eq!(r3.merged[0].corrected, "three");
    }

    #[test]
    fn duplicate_creation_on_two_devices_yields_one_winner() {
        // Both devices add "github" offline with different syncIds.
        let mut a = dict("github", "GitHub", "correction", false, 100, "dev-a");
        let mut b = dict("github", "GitHub", "correction", false, 200, "dev-b");
        a.sync_id = uuid::Uuid::new_v4().to_string();
        b.sync_id = uuid::Uuid::new_v4().to_string();
        let outcome = merge_dictionary(&[a], &[b]);
        assert_eq!(
            outcome.merged.len(),
            1,
            "business-key dedup collapses duplicates"
        );
    }

    #[test]
    fn dict_merge_preserves_one_winner_per_business_key_case_insensitive() {
        let a = dict("Hello", "hi", "correction", false, 100, "a");
        let b = dict("hello", "hi", "correction", false, 200, "a");
        let outcome = merge_dictionary(&[a], &[b.clone()]);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].updated_at, 200);
    }

    #[test]
    fn dict_merge_keeps_tombstone_forever_never_gc() {
        let tomb = dict("hello", "hi", "correction", true, 100, "a");
        let outcome = merge_dictionary(&[tomb.clone()], &[]);
        assert_eq!(outcome.merged.len(), 1);
        assert!(outcome.merged[0].deleted_at.is_some());
    }

    #[test]
    fn snippet_merge_lww_and_canonical_exact() {
        let s1 = snippet("Trigger", "Expansion", false, 100, "a");
        let s2 = snippet("trigger", "expansion", false, 200, "a");
        let outcome = merge_snippets(&[s1], &[s2.clone()]);
        assert_eq!(outcome.merged[0].expansion, "expansion");

        let del = snippet("trigger", "expansion", true, 300, "a");
        let outcome2 = merge_snippets(&[s2], &[del]);
        assert!(outcome2.merged[0].deleted_at.is_some());
    }

    // ── Settings ────────────────────────────────────────────────────────

    fn setting(key: &str, value: &str, updated_at: i64, device_id: &str) -> SettingsItem {
        SettingsItem {
            key: key.to_string(),
            value: value.to_string(),
            updated_at,
            device_id: device_id.to_string(),
        }
    }

    #[test]
    fn settings_lww_per_key() {
        let local = vec![setting("language", "en", 100, "a")];
        let remote = vec![setting("language", "fr", 200, "b")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].value, "fr");
    }

    #[test]
    fn settings_device_id_breaks_tie() {
        let local = vec![setting("language", "en", 100, "b")];
        let remote = vec![setting("language", "fr", 100, "a")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged[0].value, "en");
    }

    #[test]
    fn settings_ignores_disallowed_keys() {
        let bad = vec![setting("theme", "dark", 999, "a")];
        let good = vec![setting("language", "en", 100, "a")];
        let outcome = merge_settings(&bad, &good);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].key, "language");
    }

    #[test]
    fn adoption_loses_to_existing_remote() {
        let local = vec![setting("language", "en", 0, "a")];
        let remote = vec![setting("language", "fr", 100, "b")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].value, "fr");
        assert_eq!(outcome.merged[0].updated_at, 100);
    }

    #[test]
    fn adoption_wins_when_no_remote_and_gets_stamped() {
        let local = vec![setting("language", "en", 0, "a")];
        let remote = vec![];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].value, "en");
        assert_eq!(
            outcome.merged[0].updated_at, SETTINGS_ADOPTION_EPOCH_MS,
            "adoption sentinel must match Android's frozen stamp (1700000000000)"
        );
    }

    #[test]
    fn adoption_stamp_ties_again_resolve_by_device_id() {
        // Two platforms first-observing the same key write the identical
        // sentinel; a later re-merge must not re-stamp it.
        let a = setting("language", "en", SETTINGS_ADOPTION_EPOCH_MS, "a");
        let b = setting("language", "fr", SETTINGS_ADOPTION_EPOCH_MS, "b");
        let outcome = merge_settings(&[a.clone()], &[b.clone()]);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(
            outcome.merged[0].device_id, "b",
            "deviceId tie-breaks identical sentinels"
        );
        assert_eq!(
            outcome.merged[0].updated_at, SETTINGS_ADOPTION_EPOCH_MS,
            "no re-stamp"
        );
    }

    // ── C1 differential ──────────────────────────────────────────────────
    // merge_settings itself stamps 0 -> SETTINGS_ADOPTION_EPOCH_MS on output,
    // so that value is a first-observation marker, not a real Nov-2023
    // timestamp, and must be read back as the adoption class. These three
    // cases pin the agreed Android semantics; identical inputs on both
    // platforms must yield identical winners.

    #[test]
    fn c1_sentinel_vs_real_timestamp_remote_wins() {
        let local = vec![setting("language", "en", SETTINGS_ADOPTION_EPOCH_MS, "d1")];
        let remote = vec![setting("language", "de", 100, "d2")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(
            outcome.merged[0].value, "de",
            "a real timestamp must beat the adoption sentinel"
        );
        assert_eq!(outcome.merged[0].updated_at, 100);
    }

    #[test]
    fn c1_zero_vs_zero_adversarial_device_ids() {
        let local = vec![setting("language", "en", 0, "zzz-local")];
        let remote = vec![setting("language", "de", 0, "aaa-remote")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(
            outcome.merged[0].device_id, "zzz-local",
            "greater deviceId breaks the adoption tie"
        );
        assert_eq!(outcome.merged[0].value, "en");
        assert_eq!(
            outcome.merged[0].updated_at, SETTINGS_ADOPTION_EPOCH_MS,
            "adoption class re-stamps on output"
        );
    }

    #[test]
    fn c1_sentinel_vs_sentinel_adversarial_device_ids() {
        let local = vec![setting(
            "language",
            "en",
            SETTINGS_ADOPTION_EPOCH_MS,
            "aaa-local",
        )];
        let remote = vec![setting(
            "language",
            "de",
            SETTINGS_ADOPTION_EPOCH_MS,
            "zzz-remote",
        )];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(
            outcome.merged[0].device_id, "zzz-remote",
            "greater deviceId breaks the sentinel tie"
        );
        assert_eq!(outcome.merged[0].value, "de");
        assert_eq!(
            outcome.merged[0].updated_at, SETTINGS_ADOPTION_EPOCH_MS,
            "no re-stamp of an already-stamped value"
        );
    }

    // ── Stats ───────────────────────────────────────────────────────────

    /// A full tie on (updatedAt, deviceId) must still resolve to the SAME
    /// record on every device. If the last tiebreak is "local vs remote", two
    /// devices that each hold a different payload for that key can each keep
    /// their own copy forever and re-push to each other indefinitely.
    ///
    /// The order must be total, so the final tiebreak is syncId.
    #[test]
    fn tie_on_timestamp_and_device_resolves_by_sync_id_not_by_side() {
        let mut local = dict("hello", "LOCAL", "correction", false, 100, "dev-a");
        let mut remote = dict("hello", "REMOTE", "correction", false, 100, "dev-a");
        // Fixed ids: local sorts last, so a syncId tiebreak must pick LOCAL.
        local.sync_id = "ffffffff-ffff-4fff-8fff-ffffffffffff".to_string();
        remote.sync_id = "00000000-0000-4000-8000-000000000001".to_string();

        let out = merge_dictionary(&[local], &[remote]);
        assert_eq!(out.merged.len(), 1);
        assert_eq!(
            out.merged[0].corrected, "LOCAL",
            "a full (updatedAt, deviceId) tie must break deterministically by \
             syncId, not by whether the record happened to be local or remote"
        );
    }

    fn stat(event_id: &str, day: &str, ts: i64) -> StatsItem {
        StatsItem {
            event_id: event_id.to_string(),
            day: day.to_string(),
            timestamp_ms: ts,
            words: Some(10),
            chars: Some(40),
            duration_ms: Some(5000),
            updated_at: None,
            device_id: None,
        }
    }

    #[test]
    fn stats_union_dedup_and_cross_device_summation_inputs() {
        let e1 = stat("11111111-1111-4111-8111-111111111111", "2026-01-01", 100);
        let e2 = stat("22222222-2222-4222-8222-222222222222", "2026-01-02", 200);
        let dup = e1.clone();
        let outcome = merge_stats(&[e1.clone(), e2.clone()], &[dup]);
        assert_eq!(outcome.merged.len(), 2, "duplicate events collapse");
        let outcome2 = merge_stats(&[e1], &[e2]);
        assert_eq!(outcome2.merged.len(), 2, "distinct events union");
        let total_words: i64 = outcome2.merged.iter().filter_map(|s| s.words).sum();
        assert_eq!(total_words, 20, "display-time summation over merged set");
    }
}
