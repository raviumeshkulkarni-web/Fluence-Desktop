// Fluence sync — frozen v1.2 clock (wall UTC ms + maxSeen persisted floor)
//
// Winner ordering: max(updatedAt, deviceId), lexicographic. Tombstones are
// ordinary records — they win exactly when they are newest. This makes
// delete/re-create symmetric: a newer deletion beats an older live record,
// and a newer re-creation beats an older tombstone. Older remote state can
// never resurrect over a newer local deletion.

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
///   so a runaway clock cannot manufacture forever-dominant timestamps, but
///   stays strictly below a genuinely held remote value.
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
}
