use std::time::{Duration, Instant, SystemTime};

use crate::wake::Marker;

/// How often every open workspace is reconciled against roborev.
pub const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How often the reporter checks for an action-triggered reconciliation.
pub const WAKE_TICK: Duration = Duration::from_secs(1);

/// Decide whether this tick starts a full reconciliation pass.
pub fn scheduling_decision(
    now: Instant,
    next_full_pass: Instant,
    last_wake: Option<SystemTime>,
    marker: Marker,
) -> (bool, Option<SystemTime>) {
    if now >= next_full_pass {
        return (true, last_wake);
    }

    match marker {
        Marker::At(observed) => (Some(observed) != last_wake, Some(observed)),
        Marker::Absent => (last_wake.is_some(), None),
        Marker::Unreadable => (false, last_wake),
    }
}

pub fn observed_time(marker: Marker) -> Option<SystemTime> {
    match marker {
        Marker::At(observed) => Some(observed),
        Marker::Absent | Marker::Unreadable => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, SystemTime};

    use super::{POLL_INTERVAL, scheduling_decision};
    use crate::wake::Marker;

    #[test]
    fn marker_changes_wake_once_and_unreadable_markers_do_not() {
        let now = Instant::now();
        let later = SystemTime::UNIX_EPOCH + Duration::from_secs(2);
        let earlier = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        assert_eq!(
            scheduling_decision(now, now + POLL_INTERVAL, None, Marker::At(later)),
            (true, Some(later))
        );
        assert_eq!(
            scheduling_decision(now, now + POLL_INTERVAL, Some(later), Marker::Absent),
            (true, None)
        );
        assert_eq!(
            scheduling_decision(now, now + POLL_INTERVAL, Some(later), Marker::At(earlier)),
            (true, Some(earlier))
        );
        assert_eq!(
            scheduling_decision(now, now + POLL_INTERVAL, Some(earlier), Marker::Unreadable),
            (false, Some(earlier))
        );
    }
}
