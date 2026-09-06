use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::exec;
use crate::wake;

/// How long a child must last for its exit to count as a lost stream rather than a failed start.
const HEALTHY_RUN: Duration = Duration::from_secs(10);

/// Minimum reconnect delay, checked after any in-progress reconciliation finishes.
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

/// Own the stream child and its stdout reader until shutdown.
#[derive(Default)]
pub struct Watcher {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    started: Option<Instant>,
    next: Option<Instant>,
    failures: u32,
    last_error: Option<String>,
}

impl Watcher {
    /// Reap an exited stream and reconnect when its backoff expires.
    pub fn tick(&mut self) {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    self.join_reader();
                    let run = self.started.take().unwrap().elapsed();
                    self.last_error = Some(format!("roborev stream exited with {status}"));
                    self.retry(outcome(run));
                }
                Ok(None) => return,
                Err(err) => {
                    eprintln!("roboherd: stream status unavailable: {err}");
                    self.last_error = Some(format!("stream status unavailable: {err}"));
                    return;
                }
            }
        }
        if self.next.is_some_and(|next| Instant::now() < next) {
            return;
        }
        match exec::spawn_streaming("roborev", &["stream"]) {
            Ok(mut child) => {
                self.last_error = None;
                self.reader = child
                    .stdout
                    .take()
                    .map(|stdout| thread::spawn(move || touch_per_line(stdout)));
                self.started = Some(Instant::now());
                self.child = Some(child);
            }
            Err(err) => {
                eprintln!("roboherd: event stream unavailable: {err}");
                self.last_error = Some(err.to_string());
                self.retry(Outcome::FailedStart);
            }
        }
    }

    /// Return the current stream error, if one has been observed.
    pub fn error(&self) -> Option<String> {
        self.last_error.clone()
    }

    /// Schedule the next stream attempt.
    fn retry(&mut self, outcome: Outcome) {
        self.failures = match outcome {
            Outcome::Ran => 0,
            Outcome::FailedStart => self.failures.saturating_add(1),
        };
        self.next = Some(Instant::now() + delay_after(self.failures));
    }

    /// Join the reader after the child closes stdout.
    fn join_reader(&mut self) {
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.join_reader();
    }
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
