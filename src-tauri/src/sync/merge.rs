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

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::sync::clock::cmp_keyed_winner;
use crate::sync::domain::*;

#[derive(Debug, Clone)]
pub struct MergeOutcome<T> {
    pub merged: Vec<T>,
    pub changed: bool,
}

// ── Total winner order for keyed domains (audit #5) ──────────────────────────
//
// `clock::cmp_keyed_winner` orders records by (updatedAt, deviceId, syncId).
// That is still a PARTIAL order: a cross-key rename keeps the record's
// syncId, so two records can tie on all three keys while carrying different
// payloads. `Iterator::max_by` returns the LAST of the equal candidates, and
// candidates are folded local-then-remote, so the per-key winner depended on
// whether a record arrived as the local or the remote copy — two devices
// holding different payloads for one business key would each keep their own,
// re-push it forever, and never converge.
//
// RULE A — PER-BUSINESS-KEY WINNER (mirror this verbatim in Kotlin `Merge.kt`):
//
//   1. `updatedAt`  — larger wins.
//   2. `deviceId`   — larger wins, compared as UTF-8 bytes.
//   3. `syncId`     — larger wins, compared as UTF-8 bytes.
//   4. CANONICAL WIRE FORM of the record — larger wins. The canonical wire
//      form is the record's own serialization in frozen key order, i.e. the
//      exact bytes the frozen contract writes and Android's
//      `DomainSerializer` emits:
//        dict     {"syncId","businessKey","spoken","corrected","isEnabled",
//                  "updatedAt","deletedAt","deviceId"}
//        snippet  {"syncId","businessKey","trigger","expansion","isEnabled",
//                  "updatedAt","deletedAt","deviceId"}
//        agent    {"syncId","businessKey","name","hint","updatedAt",
//                  "deletedAt","deviceId"}
//        style    {"syncId","businessKey","name","hint","updatedAt",
//                  "deletedAt","deviceId"}
//      Kotlin MUST compare the canonical form by UNICODE CODE POINT (equivalently
//      by its UTF-8 bytes). Kotlin's `String.compareTo` is UTF-16 code-unit
//      order and DISAGREES with code-point order for supplementary-plane
//      characters, so `String.compareTo` must NOT be used here. Build the
//      canonical string with the SAME JSON writer settings as the serializer
//      (compact separators, frozen field order, `null` for an absent
//      `deletedAt`); no spaces, no reordered keys.
//   5. Records equal on all four keys are the SAME record on the wire, so they
//      compare `Equal` and fold order keeps deciding true duplicates.
//
// Levels 1–4 make Rule A TOTAL: it depends only on record CONTENT, never on
// local/remote side or iteration order. Level 4 is reached only on a genuine
// three-way tie, so the serialization cost is paid per tie, not per comparison.
// `deleted_at` is never compared — a tombstone is an ordinary state that wins
// on recency and loses to any newer edit, exactly as in every other domain.
//
// RULE B — CROSS-KEY `syncId` COLLAPSE (UNCHANGED from before audit #5, and
// already implemented in Android): after the per-key pick, records sharing a
// `syncId` under different business keys are folded by levels 1–3 of Rule A
// only; a residual full tie keeps the record that came FIRST in ascending
// business-key order, i.e. the SMALLEST business key wins. This is
// deterministic — `merged` is emitted in `BTreeMap` key order — and it is
// deliberately NOT Rule A level 4, because Android's existing pre-collapse
// sort already resolves it this way and diverging would desynchronise the two
// platforms for no correctness gain. Pinned by
// `rename_full_tie_keeps_smallest_business_key`.

/// Canonical wire form of a record, used only as Rule A's last-resort
/// tiebreak.
///
/// `to_string` cannot fail for these types (no map keys, no floats), and
/// degrades to `""` rather than panicking if a future item type ever makes it
/// failable — which would only restore the old order-dependent behaviour, not
/// lose data.
fn canonical_content<T: Serialize>(item: &T) -> String {
    serde_json::to_string(item).unwrap_or_default()
}

/// Merge dictionary items: one winner per businessKey using pure LWW.
///
/// The per-key comparison is the *total* Rule A documented above, so the
/// winner never depends on whether a record arrived as the local or the remote
/// copy, nor on iteration order. Android's `Merge` applies the identical rule.
fn merge_keyed<T, K>(
    local: &[T],
    remote: &[T],
    business_key: impl Fn(&T) -> K,
    updated_at: impl Fn(&T) -> i64,
    device_id: impl Fn(&T) -> &str,
    sync_id: impl Fn(&T) -> &str,
    mut sort_by: impl FnMut(&mut [T]),
) -> MergeOutcome<T>
where
    T: Clone + PartialEq + Serialize,
    K: Ord + Clone + Eq + std::hash::Hash,
{
    // Rule A. Folded over `local.iter().chain(remote.iter())`, so the fold
    // order is fixed but `max_by` returns the LAST of equals — without the
    // content level that would be a remote-side bias, i.e. split-brain.
    let keyed_cmp = |a: &T, b: &T| {
        cmp_keyed_winner(
            updated_at(a),
            device_id(a),
            sync_id(a),
            updated_at(b),
            device_id(b),
            sync_id(b),
        )
        .then_with(|| canonical_content(a).cmp(&canonical_content(b)))
    };
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
            .max_by(|a, b| keyed_cmp(a, b))
            .unwrap();
        match local_map.get(&bk) {
            Some(local_item) if local_item == &winner => {}
            _ => changed = true,
        }
        merged.push(winner);
    }
    // Cross-key sync_id collapse (audit finding H3 / Phase D): a rename moves
    // a record's business key while keeping its sync_id, so the pre-rename key
    // (usually remote) and the post-rename key (local) each contribute a winner
    // for the SAME logical record. Without this step both survive every future
    // merge as a permanent duplicate. Collapse by levels 1–3 of Rule A only
    // (see RULE B above); the outcome is independent of merge side/order.
    // Tombstones ride the same comparator (`deleted_at` is never compared),
    // preserving delete-vs-edit semantics across keys: the newest record wins
    // regardless of which key it sits under.
    //
    // Residual ties keep the FIRST record in ascending business-key order, so
    // the smallest business key wins — the rule Android's pre-collapse sort
    // already implements.
    let mut by_sync_id: HashMap<String, T> = HashMap::new();
    for item in merged {
        let sid = sync_id(&item).to_string();
        match by_sync_id.get(&sid) {
            None => {
                by_sync_id.insert(sid, item);
            }
            Some(incumbent) => {
                if cmp_keyed_winner(
                    updated_at(&item),
                    device_id(&item),
                    sync_id(&item),
                    updated_at(incumbent),
                    device_id(incumbent),
                    sync_id(incumbent),
                ) == Ordering::Greater
                {
                    by_sync_id.insert(sid, item);
                }
                // The collapsed set differs from any store still holding the
                // dropped row, so wholesale-replace stores (dictionary,
                // snippets) rewrite the local file with the collapsed set on
                // the `changed` write-back. Business-key-keyed reconciles
                // (agents/styles) retain winnerless local rows by design; the
                // collapse cannot fire there today because agent/style sync_ids
                // are 1:1 with their keys (`stable_sync_id`), so no heal gap
                // exists — only corrupt input could produce one.
                changed = true;
            }
        }
    }
    let mut merged: Vec<T> = by_sync_id.into_values().collect();
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

/// Settings merge: per-key LWW over the allowed keys only. Full ties resolve
/// by value, larger wins (H2: total order coordinated with Android
/// `Merge.mergeSettings`, so both platforms pick the same winner when
/// updatedAt and deviceId tie); output is canonically sorted by key,
/// matching Android exactly.
///
/// Tombstones (audit #13) are ordinary LWW states, exactly like dictionary /
/// snippet / agent / style tombstones: `deleted_at` takes part in RECENCY
/// only and is NEVER itself a tiebreak. The comparison below therefore still
/// reads `(effective updatedAt, deviceId, value)` — `value` remains the final
/// discriminator, so a full tie between a tombstone and a live record resolves
/// by value identically on both platforms and a tombstone can never win a tie
/// it did not win on recency. The newest record for a key wins outright, and
/// the merged result carries that record's `deleted_at` verbatim.
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
                    super::clock::cmp_settings_winner(
                        item_at,
                        &item.device_id,
                        &item.value,
                        existing_at,
                        &existing.device_id,
                        &existing.value,
                    ) == Ordering::Greater
                } else if existing_at == 0 {
                    true
                } else if item_at == 0 {
                    false
                } else {
                    super::clock::cmp_settings_winner(
                        item_at,
                        &item.device_id,
                        &item.value,
                        existing_at,
                        &existing.device_id,
                        &existing.value,
                    ) == Ordering::Greater
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
    // A settings record's identity is its full content. `deleted_at` is part of
    // that: a retraction changes nothing else about the record, so a set that
    // omitted it could not see one.
    //
    // DEFENSIVE, NOT CURRENTLY REACHABLE. With `value` as the comparator's last
    // level, a merged record can never differ from the local one in
    // `deleted_at` alone: if the remote wins it must have out-ranked the local
    // on `(updatedAt, deviceId, value)`, and if it only tied, local-first keeps
    // the local record verbatim. So no input today flips `changed` purely on
    // this field. It is kept so that `changed` stays a true content comparison
    // if the comparator ever gains a level that does not separate the two.
    let changed = {
        let identity = |i: &SettingsItem| {
            format!(
                "{}|{}|{}|{}",
                i.key,
                i.value,
                i.updated_at,
                i.deleted_at.map(|d| d.to_string()).unwrap_or_default()
            )
        };
        let local_set: HashSet<String> = local
            .iter()
            .filter(|i| is_allowed_settings_key(&i.key))
            .map(identity)
            .collect();
        let merged_set: HashSet<String> = merged.iter().map(identity).collect();
        local_set != merged_set
    };
    MergeOutcome { merged, changed }
}

/// Stats merge: union dedup by eventId. Normally duplicate event ids carry the
/// same immutable fact; if damaged/old data disagrees, use the same deterministic
/// LWW rule as Android so both platforms converge to one record. Full ties
/// resolve by (day, timestamp_ms, words, chars, duration_ms), larger wins
/// (H2: total order coordinated with Android `Merge.mergeStats`); identical
/// rows still tie so fold order keeps deciding true duplicates. Output is
/// canonically sorted by (day, eventId), matching Android exactly.
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
                if super::clock::cmp_stats_winner(
                    item_time,
                    item.device_id.as_deref().unwrap_or(""),
                    &item.day,
                    item.timestamp_ms,
                    item.words.unwrap_or(0),
                    item.chars.unwrap_or(0),
                    item.duration_ms.unwrap_or(0),
                    existing_time,
                    existing.device_id.as_deref().unwrap_or(""),
                    &existing.day,
                    existing.timestamp_ms,
                    existing.words.unwrap_or(0),
                    existing.chars.unwrap_or(0),
                    existing.duration_ms.unwrap_or(0),
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

    // ── Cross-key sync_id dedup (audit finding H3 / Phase D) ─────────────
    //
    // A rename moves a record's business key while keeping its sync_id, so the
    // pre-rename key (usually remote) and the post-rename key (local) can each
    // contribute a winner for the SAME logical record. These tests pin the
    // defect first: without the collapse both survive forever as a permanent
    // duplicate.

    #[test]
    fn rename_same_sync_id_different_snippet_keys_collapses_to_newest() {
        // Real-world shape (observed in production data): device B renamed
        // "my email" -> "my primary email", keeping the record id. The remote
        // still holds it under the old trigger.
        let old = snippet("my email", "a@x.com", false, 100, "a");
        let mut new = snippet("my primary email", "a@x.com", false, 200, "a");
        new.sync_id = old.sync_id.clone();
        let outcome = merge_snippets(&[new], &[old]);
        assert_eq!(
            outcome.merged.len(),
            1,
            "same sync_id under two keys must collapse to one record"
        );
        assert_eq!(outcome.merged[0].trigger, "my primary email");
    }

    #[test]
    fn rename_same_sync_id_different_dictionary_keys_collapses_to_newest() {
        let old = dict("oldword", "Old", "correction", false, 100, "a");
        let mut new = dict("newword", "Old", "correction", false, 200, "a");
        new.sync_id = old.sync_id.clone();
        let outcome = merge_dictionary(&[new], &[old]);
        assert_eq!(
            outcome.merged.len(),
            1,
            "same sync_id under two keys must collapse to one record"
        );
        assert_eq!(outcome.merged[0].spoken, "newword");
    }

    #[test]
    fn rename_collapse_is_independent_of_merge_side() {
        // The collapse uses the same total order as the per-key pick, so which
        // side a record arrived on must not matter.
        let old = dict("oldword", "Old", "correction", false, 100, "a");
        let mut new = dict("newword", "Old", "correction", false, 200, "a");
        new.sync_id = old.sync_id.clone();
        let ab = merge_dictionary(&[new.clone()], &[old.clone()]);
        let ba = merge_dictionary(&[old], &[new]);
        assert_eq!(ab.merged.len(), 1);
        assert_eq!(ba.merged.len(), 1);
        assert_eq!(ab.merged[0], ba.merged[0]);
    }

    #[test]
    fn rename_collapse_keeps_newer_tombstone_over_older_live() {
        // Delete-vs-rename uses the same newest-wins rule as delete-vs-edit:
        // deleted_at is never compared, only recency.
        let live = dict("newword", "Old", "correction", false, 100, "a");
        let mut tomb = dict("oldword", "Old", "correction", true, 200, "b");
        tomb.sync_id = live.sync_id.clone();
        let outcome = merge_dictionary(&[live], &[tomb]);
        assert_eq!(outcome.merged.len(), 1);
        assert!(
            outcome.merged[0].deleted_at.is_some(),
            "the newer tombstone must win even across keys"
        );
    }

    #[test]
    fn rename_collapse_keeps_newer_live_over_older_tombstone() {
        let tomb = dict("oldword", "Old", "correction", true, 100, "a");
        let mut live = dict("newword", "Old", "correction", false, 200, "b");
        live.sync_id = tomb.sync_id.clone();
        let outcome = merge_dictionary(&[tomb], &[live]);
        assert_eq!(outcome.merged.len(), 1);
        assert!(
            outcome.merged[0].deleted_at.is_none(),
            "the newer live record must win even across keys"
        );
        assert_eq!(outcome.merged[0].spoken, "newword");
    }

    #[test]
    fn distinct_sync_ids_still_yield_distinct_records() {
        // The collapse must ONLY fire on shared sync_id: two genuinely
        // different records (different ids) under different keys both survive.
        let a = dict("alpha", "A", "correction", false, 100, "a");
        let b = dict("beta", "B", "correction", false, 200, "a");
        let outcome = merge_dictionary(&[a, b], &[]);
        assert_eq!(outcome.merged.len(), 2);
    }

    #[test]
    fn rename_collapse_is_stable_across_repeated_passes() {
        // First pass collapses the rename; later passes — including one
        // against a stale pre-rename copy from a device that never saw the
        // rename — must stay collapsed and converge, never flap back to two.
        let old = dict("oldword", "Old", "correction", false, 100, "a");
        let mut new = dict("newword", "Old", "correction", false, 200, "a");
        new.sync_id = old.sync_id.clone();
        let first = merge_dictionary(&[new.clone()], &[old]);
        assert_eq!(first.merged.len(), 1);

        let mut stale = dict("oldword", "Old", "correction", false, 50, "c");
        stale.sync_id = new.sync_id.clone();
        let second = merge_dictionary(&first.merged, &[stale]);
        assert_eq!(
            second.merged.len(),
            1,
            "a stale pre-rename copy must not resurrect the duplicate"
        );
        assert_eq!(second.merged[0].spoken, "newword");

        let third = merge_dictionary(&second.merged, &[]);
        assert_eq!(third.merged.len(), 1);
        assert!(
            !third.changed,
            "a converged set re-merges clean with no spurious change"
        );
    }

    #[test]
    fn rename_full_tie_keeps_smallest_business_key() {
        // Same sync_id, same updated_at, same device_id under two keys: the
        // sync_id tiebreak is inert inside the group, so determinism comes
        // from key order — smallest business key wins. That is RULE B (the
        // cross-key collapse deliberately does NOT use Rule A's content
        // level, because Android's existing pre-collapse sort already resolves
        // it this way and diverging would desynchronise the platforms).
        // Practically unreachable (a rename restamps updated_at), pinned so
        // the order is total by construction.
        let old = dict("oldword", "Old", "correction", false, 100, "a");
        let mut new = dict("newword", "Old", "correction", false, 100, "a");
        new.sync_id = old.sync_id.clone();
        let ab = merge_dictionary(&[new.clone()], &[old.clone()]);
        let ba = merge_dictionary(&[old], &[new]);
        assert_eq!(ab.merged.len(), 1);
        assert_eq!(ba.merged.len(), 1);
        assert_eq!(ab.merged[0].spoken, "newword");
        assert_eq!(ba.merged[0].spoken, "newword");
    }

    // ── H3 rename (delete-old + create-new) ──────────────────────────────
    //
    // A rename retires the old row (tombstone under the OLD key when pushed,
    // hard-deleted when never pushed) and creates a new row with a FRESH
    // sync_id under the NEW key. These tests pin the merge consequences both
    // platforms must agree on (mirrored in Android MergeTest): the old entry
    // stays dead, the new entry wins, and every scenario is side-independent.

    fn dict_with_id(
        sync_id: &str,
        spoken: &str,
        corrected: &str,
        deleted: bool,
        updated_at: i64,
        device_id: &str,
    ) -> DictionaryItem {
        let mut item = dict(
            spoken,
            corrected,
            "correction",
            deleted,
            updated_at,
            device_id,
        );
        item.sync_id = sync_id.to_string();
        item
    }

    #[test]
    fn rename_tombstone_plus_new_converges_both_orders() {
        // Device A renamed hello→hallo: tombstone(S, t=200) under "hello" +
        // live(S', t=200) under "hallo". Device B still holds live(S, t=100)
        // under "hello". Both merge orders must converge to: dead old key,
        // live new key — never a resurrection, never a duplicate.
        let old_live = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            false,
            100,
            "b",
        );
        let old_dead = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            true,
            200,
            "a",
        );
        let new_live = dict_with_id(
            "22222222-2222-4222-8222-222222222222",
            "hallo",
            "Hello",
            false,
            200,
            "a",
        );
        for (local, remote) in [
            (
                vec![old_dead.clone(), new_live.clone()],
                vec![old_live.clone()],
            ),
            (
                vec![old_live.clone()],
                vec![old_dead.clone(), new_live.clone()],
            ),
        ] {
            let out = merge_dictionary(&local, &remote);
            assert_eq!(
                out.merged.len(),
                2,
                "tombstone(old) + live(new), nothing else"
            );
            let old = out
                .merged
                .iter()
                .find(|i| i.business_key() == "hello")
                .unwrap();
            assert!(old.deleted_at.is_some(), "the old entry stays dead");
            let new = out
                .merged
                .iter()
                .find(|i| i.business_key() == "hallo")
                .unwrap();
            assert!(new.deleted_at.is_none());
            assert_eq!(new.corrected, "Hello");
        }
    }

    #[test]
    fn rename_repeated_merge_is_stable_and_stale_copies_do_not_resurrect() {
        let old_dead = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            true,
            200,
            "a",
        );
        let new_live = dict_with_id(
            "22222222-2222-4222-8222-222222222222",
            "hallo",
            "Hello",
            false,
            200,
            "a",
        );
        let converged = merge_dictionary(&[old_dead.clone(), new_live.clone()], &[]).merged;
        // A stale pre-rename copy arriving late must not resurrect anything.
        let stale = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            false,
            50,
            "c",
        );
        let second = merge_dictionary(&converged, &[stale]);
        assert_eq!(second.merged.len(), 2);
        assert!(second
            .merged
            .iter()
            .find(|i| i.business_key() == "hello")
            .unwrap()
            .deleted_at
            .is_some());
        assert!(second
            .merged
            .iter()
            .find(|i| i.business_key() == "hallo")
            .unwrap()
            .deleted_at
            .is_none());
        let third = merge_dictionary(&second.merged, &[]);
        assert!(!third.changed, "a converged set re-merges clean");
    }

    #[test]
    fn rename_concurrent_same_target_is_deterministic() {
        // Two devices rename hello→hallo concurrently with different fresh
        // sync_ids. Both sides must pick the SAME winner: per-key LWW falls
        // through (updated_at, device_id) to the sync_id tiebreak.
        let a_new = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hallo",
            "A",
            false,
            200,
            "a",
        );
        let b_new = dict_with_id(
            "22222222-2222-4222-8222-222222222222",
            "hallo",
            "B",
            false,
            200,
            "a",
        );
        let ab = merge_dictionary(&[a_new.clone()], &[b_new.clone()]);
        let ba = merge_dictionary(&[b_new], &[a_new]);
        assert_eq!(ab.merged.len(), 1);
        assert_eq!(ba.merged.len(), 1);
        assert_eq!(
            ab.merged[0].corrected, ba.merged[0].corrected,
            "both orders agree"
        );
        assert_eq!(
            ab.merged[0].corrected, "B",
            "larger sync_id wins the full tie"
        );
    }

    #[test]
    fn rename_delete_vs_rename_is_deterministic() {
        // Deleter tombstones the old key at t=300; renamer tombstoned it at
        // t=200 and created the new entry at t=200. Outcome, both orders: the
        // newer tombstone holds the old key dead, the new entry lives. The
        // delete does NOT kill the renamed entry (different key), and the
        // rename does NOT resurrect the old key — deterministic, documented.
        let old_live = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            false,
            100,
            "b",
        );
        let rename_dead = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            true,
            200,
            "a",
        );
        let rename_new = dict_with_id(
            "22222222-2222-4222-8222-222222222222",
            "hallo",
            "Hello",
            false,
            200,
            "a",
        );
        let delete_dead = dict_with_id(
            "11111111-1111-4111-8111-111111111111",
            "hello",
            "Hello",
            true,
            300,
            "c",
        );
        for (local, remote) in [
            (
                vec![rename_dead.clone(), rename_new.clone()],
                vec![old_live.clone(), delete_dead.clone()],
            ),
            (vec![old_live, delete_dead], vec![rename_dead, rename_new]),
        ] {
            let out = merge_dictionary(&local, &remote);
            let old = out
                .merged
                .iter()
                .find(|i| i.business_key() == "hello")
                .unwrap();
            assert!(old.deleted_at.is_some());
            assert_eq!(old.updated_at, 300, "the newest tombstone wins the old key");
            let new = out
                .merged
                .iter()
                .find(|i| i.business_key() == "hallo")
                .unwrap();
            assert!(
                new.deleted_at.is_none(),
                "the renamed entry survives the delete"
            );
        }
    }

    // ── Settings ────────────────────────────────────────────────────────

    fn setting(key: &str, value: &str, updated_at: i64, device_id: &str) -> SettingsItem {
        SettingsItem {
            key: key.to_string(),
            value: value.to_string(),
            updated_at,
            device_id: device_id.to_string(),
            deleted_at: None,
        }
    }

    fn setting_tomb(key: &str, value: &str, updated_at: i64, device_id: &str) -> SettingsItem {
        SettingsItem {
            key: key.to_string(),
            value: value.to_string(),
            updated_at,
            device_id: device_id.to_string(),
            deleted_at: Some(updated_at),
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

    /// H2 (coordinated with Android): a full settings tie — same key, same
    /// updatedAt, same deviceId, different value — resolves by value, larger
    /// wins, identically on both platforms. Both merge orders must yield the
    /// SAME winner; side-dependent outcomes would re-push forever.
    #[test]
    fn settings_full_tie_resolves_by_value_larger_wins() {
        let local = vec![setting("language", "en", 100, "a")];
        let remote = vec![setting("language", "fr", 100, "a")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].value, "fr");
        // Side-independent: both orders pick the same winner.
        let flipped = merge_settings(&remote, &local);
        assert_eq!(flipped.merged.len(), 1);
        assert_eq!(flipped.merged[0].value, "fr");
    }

    /// H2: a full tie that reaches the value level through the adoption
    /// sentinel class also resolves by value — the sentinel maps to the
    /// adoption class for comparison but the value discriminator still
    /// applies, identically on both platforms.
    #[test]
    fn settings_sentinel_full_tie_resolves_by_value() {
        let local = vec![setting("language", "en", SETTINGS_ADOPTION_EPOCH_MS, "d")];
        let remote = vec![setting("language", "fr", SETTINGS_ADOPTION_EPOCH_MS, "d")];
        let outcome = merge_settings(&local, &remote);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(
            outcome.merged[0].value, "fr",
            "same sentinel, same device: larger value wins"
        );
        let flipped = merge_settings(&remote, &local);
        assert_eq!(flipped.merged[0].value, "fr");
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

    /// H2 (coordinated with Android): a full stats tie — same eventId, same
    /// updatedAt, same deviceId, different payload — resolves by
    /// (day, timestamp_ms, words, chars, duration_ms), larger wins,
    /// identically on both platforms. Both merge orders must yield the SAME
    /// winner. Output order is canonical (day, eventId) on both sides.
    #[test]
    fn stats_full_tie_resolves_by_payload_larger_wins() {
        let local = StatsItem {
            event_id: "11111111-1111-4111-8111-111111111111".to_string(),
            day: "2026-01-01".to_string(),
            timestamp_ms: 100,
            words: Some(10),
            chars: Some(40),
            duration_ms: Some(5000),
            updated_at: Some(1000),
            device_id: Some("d".to_string()),
        };
        let mut remote = local.clone();
        remote.words = Some(99);
        let outcome = merge_stats(&[local.clone()], &[remote.clone()]);
        assert_eq!(outcome.merged.len(), 1);
        assert_eq!(outcome.merged[0].words, Some(99));
        // Side-independent: both orders pick the same winner.
        let flipped = merge_stats(&[remote], &[local]);
        assert_eq!(flipped.merged.len(), 1);
        assert_eq!(flipped.merged[0].words, Some(99));
    }

    /// H2: a stats full tie on day resolves further down the payload —
    /// timestamp_ms before words — identically regardless of merge order.
    #[test]
    fn stats_full_tie_same_day_resolves_by_timestamp_then_words() {
        let base = StatsItem {
            event_id: "22222222-2222-4222-8222-222222222222".to_string(),
            day: "2026-03-01".to_string(),
            timestamp_ms: 100,
            words: Some(10),
            chars: Some(40),
            duration_ms: Some(5000),
            updated_at: Some(1000),
            device_id: Some("d".to_string()),
        };
        let mut later_ts = base.clone();
        later_ts.timestamp_ms = 200;
        let out = merge_stats(&[base.clone()], &[later_ts.clone()]);
        assert_eq!(out.merged[0].timestamp_ms, 200);
        let flipped = merge_stats(&[later_ts], &[base]);
        assert_eq!(flipped.merged[0].timestamp_ms, 200);
    }

    // ── #5 full tie on (updatedAt, deviceId, syncId) ───────────────────────

    /// A record pinned to a fixed identity so every field of Rule A levels 1–3
    /// ties and only the CONTENT level can decide.
    fn full_tie_dict(sync_id: &str, spoken: &str, corrected: &str) -> DictionaryItem {
        let mut item = dict(
            spoken,
            corrected,
            "correction",
            false,
            1_713_471_000_123,
            "dev",
        );
        item.sync_id = sync_id.to_string();
        item
    }

    fn full_tie_snippet(sync_id: &str, trigger: &str, expansion: &str) -> SnippetItem {
        let mut item = snippet(trigger, expansion, false, 1_713_471_000_123, "dev");
        item.sync_id = sync_id.to_string();
        item
    }

    /// #5 headline regression. Three copies of ONE record (identical
    /// `updatedAt`, `deviceId` and `syncId`) with different payloads, held by
    /// three devices. `max_by` returns the LAST of the equal candidates and
    /// candidates are folded local-then-remote, so before the content level the
    /// winner was simply "whichever side the caller's own copy sat on" —
    /// permanent split-brain, each device re-pushing its own payload forever.
    /// Every merge order must now pick the content-maximal record.
    #[test]
    fn full_tie_on_all_three_keys_resolves_by_canonical_content_not_by_side() {
        let sid = "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f01";
        let a = full_tie_dict(sid, "tie", "AAA");
        let b = full_tie_dict(sid, "tie", "BBB");
        let c = full_tie_dict(sid, "tie", "CCC");

        let ab = merge_dictionary(&[a.clone(), b.clone()], &[c.clone()]);
        let ba = merge_dictionary(&[b.clone(), c.clone()], &[a.clone()]);
        let ca = merge_dictionary(&[c.clone(), a.clone()], &[b.clone()]);
        for out in [&ab, &ba, &ca] {
            assert_eq!(out.merged.len(), 1);
            assert_eq!(
                out.merged[0].corrected, "CCC",
                "levels 1-3 tie, so the canonical wire form decides: the \
                 record whose corrected field is \"CCC\" is the largest"
            );
        }
        assert_eq!(ab.merged, ba.merged, "order independence");
        assert_eq!(ab.merged, ca.merged, "order independence");
    }

    /// Same defect, same fix, for every domain routed through `merge_keyed` —
    /// including the agent/style keyspaces whose business key is a stable id
    /// rather than derived from content.
    #[test]
    fn full_tie_resolves_by_content_in_every_keyed_domain() {
        let sid = "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f11";
        let s_a = full_tie_snippet(sid, "tie-snip", "AAA");
        let s_b = full_tie_snippet(sid, "tie-snip", "BBB");
        let s_c = full_tie_snippet(sid, "tie-snip", "CCC");
        for out in [
            merge_snippets(&[s_a.clone(), s_b.clone()], &[s_c.clone()]),
            merge_snippets(&[s_c.clone()], &[s_b.clone(), s_a.clone()]),
        ] {
            assert_eq!(out.merged.len(), 1);
            assert_eq!(out.merged[0].expansion, "CCC");
        }

        let key = "agent:123e4567-e89b-12d3-a456-426614174000".to_string();
        let mk = |hint: &str| AgentItem {
            sync_id: sid.to_string(),
            business_key: key.clone(),
            name: "Agent".to_string(),
            hint: hint.to_string(),
            updated_at: 1_713_471_000_123,
            deleted_at: None,
            device_id: "dev".to_string(),
        };
        for out in [
            merge_agents(&[mk("AAA"), mk("BBB")], &[mk("CCC")]),
            merge_agents(&[mk("CCC")], &[mk("BBB"), mk("AAA")]),
        ] {
            assert_eq!(out.merged.len(), 1);
            assert_eq!(out.merged[0].hint, "CCC");
        }

        let key = "custom:123e4567-e89b-12d3-a456-426614174000".to_string();
        let mk = |hint: &str| StyleItem {
            sync_id: sid.to_string(),
            business_key: key.clone(),
            name: "Style".to_string(),
            hint: hint.to_string(),
            updated_at: 1_713_471_000_123,
            deleted_at: None,
            device_id: "dev".to_string(),
        };
        for out in [
            merge_styles(&[mk("AAA"), mk("BBB")], &[mk("CCC")]),
            merge_styles(&[mk("CCC")], &[mk("BBB"), mk("AAA")]),
        ] {
            assert_eq!(out.merged.len(), 1);
            assert_eq!(out.merged[0].hint, "CCC");
        }
    }

    /// The content level must not disturb records that already differ on
    /// `updatedAt` / `deviceId` / `syncId`: levels 1–3 are reached first, so
    /// recency and the Android-coordinated tiebreaks keep winning exactly as
    /// before. This is the anti-regression for the frozen LWW law.
    #[test]
    fn content_level_never_overrides_recency_device_or_sync_id() {
        let mut older_but_bigger_content =
            full_tie_dict("99999999-9999-4999-8999-999999999999", "tie", "ZZZZZZZZ");
        older_but_bigger_content.updated_at = 100;
        let newer = full_tie_dict("00000000-0000-4000-8000-000000000001", "tie", "AAA");
        let out = merge_dictionary(&[newer], &[older_but_bigger_content]);
        assert_eq!(
            out.merged[0].corrected, "AAA",
            "updatedAt outranks canonical content"
        );
    }

    /// #5 corpus replay. `scenario-31` is the full-tie shape described above,
    /// in the cross-platform convergence-corpus format, so the same file can be
    /// wired into `sync::convergence::SCENARIOS` with one `include_bytes!` line.
    ///
    /// Replay is deliberately stronger than that harness: it folds EVERY
    /// permutation of the three devices (6 folds, not 3 rotations) and requires
    /// all six to agree with the corpus `expected` block.
    #[test]
    fn convergence_scenario_31_full_tie_agrees_in_every_merge_permutation() {
        const SCENARIO_31: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/sync/convergence/scenario-31.json"
        ));
        let raw: serde_json::Value = serde_json::from_slice(SCENARIO_31).expect("scenario parses");
        let devices = raw["devices"].as_object().expect("devices map");
        let mut ids: Vec<&String> = devices.keys().collect();
        ids.sort();

        let ops = |dev: &str, kind: &str| -> Vec<serde_json::Value> {
            devices[dev]
                .as_array()
                .expect("ops")
                .iter()
                .filter(|op| op["kind"] == kind)
                .cloned()
                .collect()
        };
        let to_dict = |op: &serde_json::Value| DictionaryItem {
            sync_id: op["syncId"].as_str().unwrap().to_string(),
            spoken: op["spoken"].as_str().unwrap().to_string(),
            corrected: op["corrected"].as_str().unwrap().to_string(),
            kind: "correction".to_string(),
            is_enabled: op["isEnabled"].as_bool().unwrap_or(true),
            deleted_at: op["deletedAt"].as_i64(),
            updated_at: op["updatedAt"].as_i64().unwrap(),
            device_id: op["deviceId"].as_str().unwrap().to_string(),
        };
        let to_snip = |op: &serde_json::Value| SnippetItem {
            sync_id: op["syncId"].as_str().unwrap().to_string(),
            trigger: op["trigger"].as_str().unwrap().to_string(),
            expansion: op["expansion"].as_str().unwrap().to_string(),
            is_enabled: op["isEnabled"].as_bool().unwrap_or(true),
            deleted_at: op["deletedAt"].as_i64(),
            updated_at: op["updatedAt"].as_i64().unwrap(),
            device_id: op["deviceId"].as_str().unwrap().to_string(),
        };

        let mut folds = 0usize;
        for perm in [
            [0usize, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut dict_acc: Vec<DictionaryItem> = Vec::new();
            let mut snip_acc: Vec<SnippetItem> = Vec::new();
            for step in perm {
                let dev = ids[step];
                for op in ops(dev, "dict") {
                    dict_acc = merge_dictionary(&dict_acc, &[to_dict(&op)]).merged;
                }
                for op in ops(dev, "snip") {
                    snip_acc = merge_snippets(&snip_acc, &[to_snip(&op)]).merged;
                }
            }
            folds += 1;

            let want = &raw["expected"]["dictionary"][0];
            assert_eq!(
                dict_acc.len(),
                1,
                "fold {folds}: one winner per business key"
            );
            assert_eq!(dict_acc[0].sync_id, want["syncId"].as_str().unwrap());
            assert_eq!(
                dict_acc[0].business_key(),
                want["businessKey"].as_str().unwrap()
            );
            assert_eq!(dict_acc[0].corrected, want["corrected"].as_str().unwrap());
            assert_eq!(dict_acc[0].updated_at, want["updatedAt"].as_i64().unwrap());
            assert_eq!(dict_acc[0].device_id, want["deviceId"].as_str().unwrap());

            let want = &raw["expected"]["snippets"][0];
            assert_eq!(
                snip_acc.len(),
                1,
                "fold {folds}: one winner per business key"
            );
            assert_eq!(snip_acc[0].sync_id, want["syncId"].as_str().unwrap());
            assert_eq!(
                snip_acc[0].business_key(),
                want["businessKey"].as_str().unwrap()
            );
            assert_eq!(snip_acc[0].expansion, want["expansion"].as_str().unwrap());
            assert_eq!(snip_acc[0].updated_at, want["updatedAt"].as_i64().unwrap());
            assert_eq!(snip_acc[0].device_id, want["deviceId"].as_str().unwrap());
        }
        assert_eq!(folds, 6, "every device permutation must be exercised");
    }

    // ── #13 settings tombstones ────────────────────────────────────────────

    /// #13 headline: Android retracts a key (reset `ai_polish_style`), Windows
    /// holds the old live value. The newest record wins, so the tombstone
    /// applies on Windows and the merged result CARRIES it — which is what
    /// makes the retraction propagate back instead of being resurrected.
    #[test]
    fn android_settings_tombstone_applies_on_windows() {
        let local = vec![setting(
            "ai_polish_style",
            "formal",
            1_713_471_000_123,
            "windows",
        )];
        let remote = vec![setting_tomb(
            "ai_polish_style",
            "default",
            1_713_471_000_999,
            "android",
        )];
        for out in [
            merge_settings(&local, &remote),
            merge_settings(&remote, &local),
        ] {
            assert_eq!(out.merged.len(), 1);
            assert!(
                out.merged[0].is_tombstone(),
                "the newer retraction must win on recency alone"
            );
            assert_eq!(out.merged[0].deleted_at, Some(1_713_471_000_999));
            assert_eq!(out.merged[0].value, "default");
        }
    }

    /// A tombstone is an ordinary LWW state, so a newer write resurrects the
    /// key exactly as it does for dictionary/snippet tombstones.
    #[test]
    fn newer_settings_write_beats_an_older_tombstone() {
        let tomb = vec![setting_tomb("language", "en", 100, "android")];
        let live = vec![setting("language", "fr", 200, "windows")];
        for out in [merge_settings(&live, &tomb), merge_settings(&tomb, &live)] {
            assert_eq!(out.merged.len(), 1);
            assert_eq!(out.merged[0].value, "fr");
            assert!(
                !out.merged[0].is_tombstone(),
                "the newer live write must resurrect the key"
            );
            assert_eq!(out.merged[0].deleted_at, None);
        }
    }

    /// #13's second requirement: a tombstone is NEVER itself a tiebreak. With
    /// `updatedAt` and `deviceId` tied, the winner is still decided by `value`
    /// — the H2 rule Android already implements — so tombstone-ness cannot
    /// change the outcome and the two platforms cannot disagree about it.
    #[test]
    fn settings_tombstone_is_never_a_tiebreak_on_a_full_tie() {
        let live_low = setting("language", "en", 100, "d");
        let live_high = setting("language", "zz", 100, "d");
        let tomb_low = setting_tomb("language", "aa", 100, "d");
        let tomb_high = setting_tomb("language", "yy", 100, "d");

        // Tombstone with the SMALLER value loses to a live record with the
        // larger one, even though the live record is not a retraction.
        for out in [
            merge_settings(&[live_high.clone()], &[tomb_low.clone()]),
            merge_settings(&[tomb_low], &[live_high]),
        ] {
            assert_eq!(out.merged[0].value, "zz");
            assert!(!out.merged[0].is_tombstone());
        }
        // Symmetrically: a tombstone with the LARGER value wins.
        for out in [
            merge_settings(&[tomb_high.clone()], &[live_low.clone()]),
            merge_settings(&[live_low], &[tomb_high]),
        ] {
            assert_eq!(out.merged[0].value, "yy");
            assert!(out.merged[0].is_tombstone());
        }
    }

    /// A remote retraction must reach local state AND be reported as a state
    /// change, or the device keeps the retracted key forever and re-adopts it on
    /// the next pass. (The `changed` flip here is carried by `updatedAt`, not by
    /// `deleted_at` — see the note on `merge_settings`'s dirty check.)
    #[test]
    fn a_tombstone_only_change_is_reported_as_changed() {
        let live = vec![setting("language", "en", 100, "d")];
        let tomb = vec![setting_tomb("language", "en", 200, "d")];
        let out = merge_settings(&live, &tomb);
        assert!(out.changed, "a pure retraction is a state change");
        // ... and it settles: re-merging the merged set reports no change.
        let settled = merge_settings(&out.merged, &[]);
        assert!(
            !settled.changed,
            "a converged settings set must re-merge clean"
        );
        assert!(settled.merged[0].is_tombstone());
    }
}
