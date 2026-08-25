use std::io::{BufRead, BufReader};
use std::process::ChildStdout;
use std::thread;
use std::time::{Duration, Instant};

use crate::exec;
use crate::wake;

/// How long a child must last for its exit to count as a lost stream rather than a failed start.
const HEALTHY_RUN: Duration = Duration::from_secs(10);

/// How long to wait after a stream that had been running, matching the reporter's own tick.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Ceiling on the backoff after repeated failed starts. Without one a missing `roborev` becomes one
/// spawn per second forever.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// How many consecutive failures still lengthen the wait, past which the ceiling applies.
const BACKOFF_STEPS: u32 = 5;

/// What became of one stream child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Lasted long enough to have been connected.
    Ran,
    /// Exited at once, or never started at all.
    FailedStart,
}

/// Follow roborev's event stream on a background thread, touching the wake marker on every line.
///
/// The stream is unfiltered because one reporter serves every workspace, and a `--repo` filter would
/// mean one child per repo.
pub fn watch() {
    thread::spawn(|| {
        let mut failures: u32 = 0;
        loop {
            match follow() {
                Outcome::Ran => failures = 0,
                Outcome::FailedStart => failures = failures.saturating_add(1),
            }
            thread::sleep(delay_after(failures));
        }
    });
}

/// Run one stream child to its end.
fn follow() -> Outcome {
    let mut child = match exec::spawn_streaming("roborev", &["stream"]) {
        Ok(child) => child,
        Err(err) => {
            eprintln!("roboherd: event stream unavailable: {err}");
            return Outcome::FailedStart;
        }
    };

    let started = Instant::now();
    if let Some(stdout) = child.stdout.take() {
        touch_per_line(stdout);
    }
    // Reaped so a dead daemon does not leave a zombie behind every reconnect.
    let _ = child.wait();

    outcome(started.elapsed())
}

/// Touch the wake marker once per line until the child closes its stdout.
///
/// Lines are never parsed. The marker collapses a burst into a single pass, and `roborev list` stays
/// the source of truth for what that pass reports.
fn touch_per_line(stdout: ChildStdout) {
    let mut reader = BufReader::new(stdout);
    // Bytes rather than lines: a completed review carries its whole text, and invalid UTF-8 must not
    // end the stream.
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let _ = wake::touch();
            }
        }
    }
}

/// Classify a finished child by how long it lasted.
fn outcome(run: Duration) -> Outcome {
    if run < HEALTHY_RUN {
        Outcome::FailedStart
    } else {
        Outcome::Ran
    }
}

/// How long to wait before the next child, after this many consecutive failed starts.
///
/// `roborev stream` starts the daemon itself, so a child that lost the race against it coming up is
/// worth retrying in seconds. The ramp keeps a permanently broken daemon from being handed a spawn
/// every second on the same path.
fn delay_after(failures: u32) -> Duration {
    if failures == 0 {
        return RECONNECT_DELAY;
    }

    RECONNECT_DELAY
        .saturating_mul(1 << failures.min(BACKOFF_STEPS))
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{HEALTHY_RUN, MAX_RETRY_DELAY, Outcome, RECONNECT_DELAY, delay_after, outcome};

    #[test]
    fn a_stream_that_ran_is_not_a_failed_start() {
        assert_eq!(outcome(HEALTHY_RUN), Outcome::Ran);
        assert_eq!(outcome(Duration::from_millis(20)), Outcome::FailedStart);
    }

    #[test]
    fn a_stream_that_ran_reconnects_promptly() {
        assert_eq!(delay_after(0), RECONNECT_DELAY);
    }

    /// A daemon restarted underneath the stream is back within seconds, and the child that lost the
    /// race against it must not wait out the ceiling.
    #[test]
    fn the_first_failed_start_retries_in_seconds() {
        assert!(
            delay_after(1) <= Duration::from_secs(4),
            "{:?}",
            delay_after(1)
        );
    }

    /// A missing roborev never recovers, so the wait has to settle rather than grow without bound.
    #[test]
    fn repeated_failed_starts_settle_at_the_ceiling() {
        assert_eq!(delay_after(BACKOFF_CEILING_PROBE), MAX_RETRY_DELAY);
        assert_eq!(delay_after(u32::MAX), MAX_RETRY_DELAY);
    }

    /// Far enough past the ramp to be capped, without relying on the step count.
    const BACKOFF_CEILING_PROBE: u32 = 20;
}
