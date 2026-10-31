// Fluence sync - frozen v1.2 clock (wall UTC ms + maxSeen persisted floor)
//
// Winner ordering: max(updatedAt, deviceId), lexicographic. Tombstones are
// ordinary records - they win exactly when they are newest. This makes
// delete/re-create symmetric: a newer deletion beats an older live record,
// and a newer re-creation beats an older tombstone. Older remote state can
// never resurrect over a newer local deletion.
//
// Known residual (audit H1 headline, deliberately out of scope for F2):
// ingest still accepts far-ahead peer timestamps (validation tolerance), and
// those raise max_seen, so local edits — stamped at most wall +
// MAX_CLOCK_SKEW_MS — keep losing LWW for the skew duration. F2 closed the
// stats floor-field divergence and the false "strictly below" doc, not the
// tolerance itself; changing ingest acceptance would alter the frozen wire
// contract.

use std::cmp::Ordering;

/// Returns wall time in ms since epoch.
pub fn wall_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Upper bound on how far a locally-stamped updatedAt may run ahead of real
/// wall time (15 minutes). Without this cap, a wall-clock jump into the future
/// (user/NTP/clock drift) makes `max_seen` sticky: every subsequent local edit
/// is stamped with an absurd updatedAt that permanently outranks legitimate
/// remote state, wedging LWW convergence on every device.
pub const MAX_CLOCK_SKEW_MS: i64 = 900_000;

/// Monotonic timestamp derived from wall clock and persisted maxSeen.
/// Ensures updatedAt is strictly increasing per device even if the wall clock
/// jumps backwards, while capping how far ahead of real wall time a stamp may
/// run (sticky future-clock guard, see [`MAX_CLOCK_SKEW_MS`]).
///
/// Returns `(stamp, max_seen_floor)`:
/// - `stamp` (written into the record) is capped at `wall + MAX_CLOCK_SKEW_MS`
///   so a runaway clock cannot manufacture forever-dominant timestamps. A
///   small legitimately-future held value is NOT capped away: when
///   `seen + 1 <= wall + MAX_CLOCK_SKEW_MS` the stamp is `seen + 1`, i.e. it
///   stays strictly ABOVE the held remote value and keeps local monotonicity.
///   Only a runaway floor (beyond `wall + MAX_CLOCK_SKEW_MS`) is pulled down
///   to the cap.
/// - `max_seen_floor` stays raw-and-monotone so the persisted floor never
///   decreases; `update_max_seen` only ever raises it.
pub fn monotonic_now(max_seen: i64) -> (i64, i64) {
    let wall = wall_now_ms();
    let next = wall.max(max_seen + 1);
    let capped = next.min(wall + MAX_CLOCK_SKEW_MS);
    (capped, next)
}

/// Winner comparison for dictionary/snippet/settings/stats records.
/// Ordering is lexicographic: updatedAt (newer > older), then deviceId
/// (lexicographically larger wins) as the deterministic tiebreak.
pub fn cmp_winner(
    a_updated_at: i64,
    a_device_id: &str,
    b_updated_at: i64,
    b_device_id: &str,
) -> Ordering {
    a_updated_at
        .cmp(&b_updated_at)
        .then_with(|| a_device_id.cmp(b_device_id))
}

/// LWW for keyed domains (dictionary, snippets) with a final `sync_id`
/// tiebreak, making the order *total*.
///
/// `cmp_winner` alone is not a total order: two records can share both
/// `updated_at` and `device_id` while differing in payload. A partial order
/// makes the merge outcome depend on which side a record arrived from, so two
/// devices holding different payloads for the same key each keep their own copy
/// and re-push to each other indefinitely. `sync_id` is stable, present on every
/// record, and identical on both platforms and both sides of the wire, so it is
/// a safe final discriminator.
pub fn cmp_keyed_winner(
    a_updated_at: i64,
    a_device_id: &str,
    a_sync_id: &str,
    b_updated_at: i64,
    b_device_id: &str,
    b_sync_id: &str,
) -> Ordering {
    cmp_winner(a_updated_at, a_device_id, b_updated_at, b_device_id)
        .then_with(|| a_sync_id.cmp(b_sync_id))
}

/// LWW for the settings domain with a final `value` tiebreak, making the
/// order *total* (audit H2, coordinated with Android).
///
/// `cmp_winner` alone leaves a full tie — same key, same `updated_at`, same
/// `device_id`, different `value` — to merge side/fold order, so two devices
/// each keep their own value and re-push forever. `value` is stable, present
/// on every record, and wire-identical on both platforms, so it is a safe
/// final discriminator. Larger wins at every level, matching the `device_id`
/// convention. Callers pass *effective* times (adoption sentinel already
/// mapped); identical rows still compare `Equal`, preserving local-first
/// fold behavior for true duplicates.
pub fn cmp_settings_winner(
    a_updated_at: i64,
    a_device_id: &str,
    a_value: &str,
    b_updated_at: i64,
    b_device_id: &str,
    b_value: &str,
) -> Ordering {
    cmp_winner(a_updated_at, a_device_id, b_updated_at, b_device_id)
        .then_with(|| a_value.cmp(b_value))
}

/// LWW for stats records with final payload tiebreaks, making the order
/// *total* (audit H2, coordinated with Android).
///
/// Discriminator order after `(updated_at, device_id)`: `day`, then
/// `timestamp_ms`, `words`, `chars`, `duration_ms` — every field present on
/// both platforms and wire-identical. Larger wins at every level. Callers
/// normalize absent optionals to `0` (comparison only): a missing key on the
/// wire reads as the Android default `0`, so `None` and `Some(0)` compare
/// equal — convergence only needs both sides to compute the same comparison,
/// which they do. Identical rows still compare `Equal`, so fold order
/// (local-first) keeps deciding true duplicates and the upload tie-guard is
/// unaffected.
#[allow(clippy::too_many_arguments)]
pub fn cmp_stats_winner(
    a_updated_at: i64,
    a_device_id: &str,
    a_day: &str,
    a_timestamp_ms: i64,
    a_words: i64,
    a_chars: i64,
    a_duration_ms: i64,
    b_updated_at: i64,
    b_device_id: &str,
    b_day: &str,
    b_timestamp_ms: i64,
    b_words: i64,
    b_chars: i64,
    b_duration_ms: i64,
) -> Ordering {
    cmp_winner(a_updated_at, a_device_id, b_updated_at, b_device_id)
        .then_with(|| a_day.cmp(b_day))
        .then_with(|| a_timestamp_ms.cmp(&b_timestamp_ms))
        .then_with(|| a_words.cmp(&b_words))
        .then_with(|| a_chars.cmp(&b_chars))
        .then_with(|| a_duration_ms.cmp(&b_duration_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winner_newer_updated_wins() {
        let ord = cmp_winner(300, "a", 200, "z");
        assert_eq!(ord, Ordering::Greater);
        let ord2 = cmp_winner(200, "z", 300, "a");
        assert_eq!(ord2, Ordering::Less);
    }

    #[test]
    fn winner_device_id_breaks_tie() {
        let ord = cmp_winner(100, "b", 100, "a");
        assert_eq!(ord, Ordering::Greater);
        let ord2 = cmp_winner(100, "a", 100, "b");
        assert_eq!(ord2, Ordering::Less);
        let ord3 = cmp_winner(100, "same", 100, "same");
        assert_eq!(ord3, Ordering::Equal);
    }

    // H2 shared vectors (mirrored in Android ClockTest/MergeTest): a full
    // (updated_at, device_id) tie with different values resolves by value,
    // larger wins, independent of argument order — both platforms agree.
    #[test]
    fn settings_full_tie_resolves_by_value_larger_wins() {
        assert_eq!(
            cmp_settings_winner(100, "dev", "light", 100, "dev", "dark"),
            Ordering::Greater
        );
        assert_eq!(
            cmp_settings_winner(100, "dev", "dark", 100, "dev", "light"),
            Ordering::Less
        );
        assert_eq!(
            cmp_settings_winner(100, "dev", "dark", 100, "dev", "dark"),
            Ordering::Equal
        );
        // Earlier levels still dominate: newer wins regardless of value.
        assert_eq!(
            cmp_settings_winner(200, "dev", "aaa", 100, "dev", "zzz"),
            Ordering::Greater
        );
    }

    // H2 shared vectors (mirrored on Android): stats full tie resolves by
    // day, then timestamp_ms, words, chars, duration_ms — larger wins.
    #[test]
    fn stats_full_tie_resolves_by_day_then_payload() {
        // (at, dev, day, ts, words, chars, dur)
        let base = (100, "dev", "2026-01-01", 1000, 10, 50, 5000);
        let later_day = (100, "dev", "2026-01-02", 1000, 10, 50, 5000);
        let more_words = (100, "dev", "2026-01-01", 1000, 11, 50, 5000);
        let w = |r: (i64, &str, &str, i64, i64, i64, i64),
                 o: (i64, &str, &str, i64, i64, i64, i64)| {
            cmp_stats_winner(
                r.0, r.1, r.2, r.3, r.4, r.5, r.6, o.0, o.1, o.2, o.3, o.4, o.5, o.6,
            )
        };
        assert_eq!(
            w(base, later_day),
            Ordering::Less,
            "later day wins the full tie"
        );
        assert_eq!(
            w(base, more_words),
            Ordering::Less,
            "more words wins when day ties"
        );
        assert_eq!(
            w(base, base),
            Ordering::Equal,
            "identical rows still tie so fold order decides true duplicates"
        );
        // Earlier levels still dominate.
        assert_eq!(
            w((200, "dev", "2020-01-01", 0, 0, 0, 0), later_day),
            Ordering::Greater
        );
    }

    #[test]
    fn monotonic_now_monotonic() {
        // wall clock is comfortably after max_seen here, so no cap engages.
        let (next, floor) = monotonic_now(1000);
        let wall = wall_now_ms();
        assert!(next > 1000);
        assert!(next <= wall + MAX_CLOCK_SKEW_MS, "stamp respects the cap");
        assert_eq!(floor, next, "floor stays monotone when no cap engages");
        let (next2, _) = monotonic_now(floor);
        assert!(next2 > floor);
    }

    #[test]
    fn sticky_future_max_seen_is_capped_but_stays_monotone() {
        // Simulate a wall-clock jump far into the future: max_seen is now
        // enormous. The stamp must be capped at wall + skew, never "sticky".
        // The monotone floor (max_seen) is preserved as-is; persistence only
        // ever raises it via update_max_seen.
        let wall = wall_now_ms();
        let runaway_max_seen = wall + 3 * MAX_CLOCK_SKEW_MS; // 45 min ahead
        let (stamp, floor) = monotonic_now(runaway_max_seen);
        // `monotonic_now` reads its own wall time, which may be a few ms after
        // our captured `wall`; assert within a tolerance instead of exactly.
        assert!(
            stamp >= wall + MAX_CLOCK_SKEW_MS && stamp <= wall + MAX_CLOCK_SKEW_MS + 1_000,
            "a runaway max_seen must not inflate the new edit's stamp beyond wall+skew"
        );
        assert!(
            floor >= runaway_max_seen,
            "the persisted floor stays monotone (never lowered)"
        );

        // A legitimately-held remote value slightly ahead (accurate peer clock)
        // is NOT over-capped: a small positive skew stays dominant.
        let small_skew = wall + 120_000; // 2 min ahead
        let (stamp3, _) = monotonic_now(small_skew);
        assert_eq!(stamp3, small_skew + 1);
    }

    #[test]
    fn stamps_stay_monotone_while_wall_lags_the_floor() {
        // Post-jump state: the persisted floor sits far ahead of wall (after
        // either a forward wall jump or a backward one). Successive stamps
        // must not regress into an LWW loss, while every stamp itself stays
        // capped at wall + skew. Note the floor — not wall — is what keeps
        // stamps above previously observed data; the cap binds both calls.
        let wall = wall_now_ms();
        let floor_before_jump = wall + 30 * 60_000; // 30 min ahead
        let (stamp1, floor1) = monotonic_now(floor_before_jump);
        let (stamp2, floor2) = monotonic_now(floor1);
        assert!(floor1 >= floor_before_jump, "floor never lowered");
        assert!(floor2 > floor1, "raw floor keeps rising across calls");
        assert!(
            stamp2 >= stamp1,
            "stamps never regress after a backward jump"
        );
        assert!(
            stamp2 <= wall_now_ms() + MAX_CLOCK_SKEW_MS + 1_000,
            "the future-clock cap still holds"
        );
    }
}
