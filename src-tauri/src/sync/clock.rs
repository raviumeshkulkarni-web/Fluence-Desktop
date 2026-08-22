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

/// Monotonic timestamp derived from wall clock and persisted maxSeen.
/// Ensures updatedAt is strictly increasing per device, even if the wall
/// clock jumps backwards.
pub fn monotonic_now(max_seen: i64) -> (i64, i64) {
    let wall = wall_now_ms();
    let next = wall.max(max_seen + 1);
    (next, next)
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
        let (next, max_seen) = monotonic_now(1000);
        assert!(next > 1000);
        assert_eq!(next, max_seen);
        let (next2, _) = monotonic_now(max_seen);
        assert!(next2 > max_seen);
    }
}
