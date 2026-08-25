use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::process::getuid;

use crate::badge::Badge;
use crate::error::{Error, Result};
use crate::git;
use crate::herdr;
use crate::reporter::stream;
use crate::reporter::transitions::{Event, TransitionTracker, summarize};
use crate::roborev;
use crate::wake::{self, Marker};

/// How often every open workspace is reconciled against roborev.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How often the reporter checks for an action-triggered reconciliation.
const WAKE_TICK: Duration = Duration::from_secs(1);

/// How long a published token survives without a refresh. Spanning roughly three polls keeps one
/// slow pass from blinking the badge off, while a dead reporter still stops showing stale state
/// within a minute.
const TOKEN_TTL: Duration = Duration::from_secs(45);

/// How long any one child process on the poll path gets before it is killed.
///
/// Six sequential commands fit inside [`TOKEN_TTL`]. Workspaces run concurrently, and a detached
/// daemon survives a timed-out `roborev list` for the next pass.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// How many jobs a badge pass reads. The badge counts recent activity, and this runs on every poll,
/// so the window stays small.
const BADGE_JOBS: usize = 50;

/// One pass's shared state, threaded through every workspace it reconciles.
struct Pass<'a> {
    /// Stamped once and shared by every workspace's metadata write.
    seq: u64,
    verbose: bool,
    tracker: &'a TransitionTracker,
    /// Transitions from every workspace, notified once when the pass joins.
    events: Mutex<Vec<Event>>,
    /// The pass's panes, grouped by workspace. Taken from the same snapshot as the workspaces.
    panes: HashMap<String, Vec<herdr::Pane>>,
}

/// Run the reconcile loop until the process is killed. One reporter serves every workspace, and a
/// failure in one workspace never ends the loop.
///
/// A second reporter would race metadata writes and make badges alternate between snapshots.
/// `--once` skips the lock because it performs one diagnostic pass.
pub fn run(once: bool, verbose: bool) -> Result<()> {
    // Held for the whole run. Dropping it early would let a second reporter in mid-loop.
    let _lock = if once { None } else { Some(claim()?) };
    let tracker = TransitionTracker::default();

    if once {
        if let Err(err) = reconcile_all(verbose, &tracker) {
            eprintln!("roboherd: snapshot failed: {err}");
        }
        return Ok(());
    }

    // Started after the lock, so only the reporter that won it holds a stream child.
    stream::watch();

    let mut next_full_pass = Instant::now();
    let mut last_wake = observed_time(wake::observe());

    loop {
        let started = Instant::now();
        let (run_pass, observed) = scheduling_decision(
            started,
            next_full_pass,
            last_wake,
            if started >= next_full_pass {
                Marker::Unreadable
            } else {
                wake::observe()
            },
        );
        last_wake = observed;
        if run_pass {
            if let Err(err) = reconcile_all(verbose, &tracker) {
                eprintln!("roboherd: snapshot failed: {err}");
            }
            next_full_pass = started + POLL_INTERVAL;
        }
        thread::sleep(WAKE_TICK);
    }
}

/// Decide whether this tick starts a full reconciliation pass.
fn scheduling_decision(
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

fn observed_time(marker: Marker) -> Option<SystemTime> {
    match marker {
        Marker::At(observed) => Some(observed),
        Marker::Absent | Marker::Unreadable => None,
    }
}

/// The file whose lock marks the running reporter.
///
/// The uid prevents account collisions in shared `/tmp`. Avoiding environment-derived directories
/// keeps one account from acquiring different locks under different environments.
///
/// The predictable path is unsafe on a shared host because another account can pre-create it or
/// replace it with a symlink. The current threat model accepts that risk on single-account hosts.
fn lock_path() -> PathBuf {
    let uid = getuid().as_raw();
    PathBuf::from("/tmp").join(format!("roboherd-reporter-{uid}.lock"))
}

/// Take the single-reporter lock, or report that another reporter holds it.
///
/// The kernel releases the lock when the process ends, avoiding cleanup, PID reuse, and
/// check-then-write races.
fn claim() -> Result<File> {
    let file = File::create(lock_path())?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            let _ = herdr::notify(
                "roboherd: reporter already running",
                "Another reporter is publishing review state. Stop it before starting another.",
                COMMAND_TIMEOUT,
            );
            Err(Error::ReporterAlreadyRunning)
        }
        Err(TryLockError::Error(source)) => Err(Error::Io(source)),
    }
}

/// Reconcile every open workspace in one pass.
fn reconcile_all(verbose: bool, tracker: &TransitionTracker) -> Result<()> {
    let seq = pass_sequence();
    let herdr::Snapshot { workspaces, panes } = herdr::snapshot(COMMAND_TIMEOUT)?;
    if verbose {
        eprintln!("roboherd: {} workspace(s)", workspaces.len());
    }

    let pass = Pass {
        seq,
        verbose,
        tracker,
        events: Mutex::new(Vec::new()),
        panes: herdr::panes_by_workspace(panes),
    };

    // Reconcile workspaces concurrently. Each one spawns at least one child process and waits on
    // the roborev daemon, so a serial pass over several workspaces can outlast the token TTL.
    thread::scope(|scope| {
        for workspace in &workspaces {
            let pass = &pass;
            scope.spawn(move || {
                if let Err(err) = reconcile(workspace, pass) {
                    eprintln!(
                        "roboherd: workspace {} ({}) failed: {err}",
                        workspace.workspace_id, workspace.label
                    );
                }
            });
        }
    });

    // Raised after the scope joins so notification latency cannot delay this pass's metadata writes.
    notify_pass(
        pass.events.into_inner().expect("pass events poisoned"),
        verbose,
    );
    Ok(())
}

/// Raise the pass's single toast.
///
/// Failure is logged and dropped. herdr reports `disabled`, `rate_limited`, `no_foreground_client`,
/// and `busy` as a command that exits zero having shown nothing, so a missed toast already looks
/// like a delivered one. The `$roborev` token is the durable half and is published either way.
fn notify_pass(events: Vec<Event>, verbose: bool) {
    let Some((title, body)) = summarize(&events) else {
        return;
    };
    if verbose {
        eprintln!("roboherd: notify {title}: {body}");
    }
    if let Err(err) = herdr::notify(title, &body, COMMAND_TIMEOUT) {
        eprintln!("roboherd: review notification failed: {err}");
    }
}

/// Milliseconds since the Unix epoch, stamped once per pass and shared by every workspace write.
///
/// Herdr retains accepted sequences across reporter restarts, so a process-local counter could
/// restart below the stored value.
fn pass_sequence() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// Publish or clear one workspace's `$roborev` token, and record its transitions for the pass.
fn reconcile(workspace: &herdr::Workspace, pass: &Pass) -> Result<()> {
    let Some(checkout) = checkout_for(workspace, pass) else {
        if pass.verbose {
            eprintln!(
                "  {} ({}): no checkout",
                workspace.workspace_id, workspace.label
            );
        }
        return Ok(());
    };

    let branch = git::current_branch(&checkout, COMMAND_TIMEOUT)?;
    let repository = git::common_dir(&checkout, COMMAND_TIMEOUT)?;
    let jobs = roborev::list(&checkout, Some(&branch), BADGE_JOBS, COMMAND_TIMEOUT)?;
    let transitions = pass
        .tracker
        .observe(&repository, &branch, &workspace.label, pass.seq, &jobs);
    if !transitions.is_empty() {
        pass.events
            .lock()
            .expect("pass events poisoned")
            .extend(transitions);
    }

    let tokens = Badge::from_jobs(&jobs).tokens();

    if pass.verbose {
        // The same separator herdr draws between adjacent tokens.
        let set: Vec<&str> = tokens.iter().flatten().map(String::as_str).collect();
        let rendered = if set.is_empty() {
            "(cleared)".to_string()
        } else {
            set.join(" · ")
        };
        eprintln!(
            "  {} ({}): {} -> {rendered}",
            workspace.workspace_id,
            workspace.label,
            checkout.display(),
        );
    }

    herdr::report_tokens(
        &workspace.workspace_id,
        tokens,
        pass.seq,
        TOKEN_TTL,
        COMMAND_TIMEOUT,
    )
}

/// Resolve the checkout to run roborev in. Only worktree workspaces carry a path of their own, so
/// an ordinary workspace falls back to its representative pane's cwd, normalized to the repo root.
///
/// The panes come from the pass's snapshot, so a workspace with none left is simply absent rather
/// than an error.
fn checkout_for(workspace: &herdr::Workspace, pass: &Pass) -> Option<PathBuf> {
    if let Some(path) = workspace.worktree_checkout() {
        return git::repo_root(&path, COMMAND_TIMEOUT);
    }

    let panes = pass.panes.get(&workspace.workspace_id)?;
    let cwd = herdr::representative_pane(panes, &workspace.active_tab_id)?;
    git::repo_root(&cwd, COMMAND_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    use super::{POLL_INTERVAL, scheduling_decision};
    use crate::reporter::transitions::{Event, Transition, TransitionTracker, summarize};
    use crate::roborev::{JobStatus, ReviewJob};
    use crate::wake::Marker;

    const CHECKOUT: &str = "/repo";
    const BRANCH: &str = "main";
    const WORKSPACE: &str = "brewstack";

    fn job(id: i64, status: JobStatus, verdict: Option<&str>) -> ReviewJob {
        ReviewJob {
            id,
            status,
            closed: None,
            verdict: verdict.map(str::to_string),
        }
    }

    /// Observe one pass in the default workspace, from the default checkout and branch.
    ///
    /// Each call is its own pass, since a key accepts one snapshot per pass. Tests that need two
    /// workspaces inside one pass call [`TransitionTracker::observe`] with a shared sequence.
    fn observe(tracker: &TransitionTracker, jobs: &[ReviewJob]) -> Vec<Event> {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        tracker.observe(Path::new(CHECKOUT), BRANCH, WORKSPACE, seq, jobs)
    }

    /// Prime the checkout so later passes report transitions.
    fn prime(tracker: &TransitionTracker, jobs: &[ReviewJob]) {
        assert!(observe(tracker, jobs).is_empty(), "priming pass is silent");
    }

    fn started(workspace: &str, job_id: i64) -> Event {
        Event {
            workspace: workspace.to_string(),
            job_id,
            transition: Transition::Started,
            status: JobStatus::Queued,
            verdict: None,
        }
    }

    fn finished(workspace: &str, job_id: i64, status: JobStatus, verdict: Option<&str>) -> Event {
        Event {
            workspace: workspace.to_string(),
            job_id,
            transition: Transition::Finished,
            status,
            verdict: verdict.map(str::to_string),
        }
    }

    #[test]
    fn the_first_pass_reports_nothing_it_finds() {
        let tracker = TransitionTracker::default();

        assert!(
            observe(
                &tracker,
                &[
                    job(7, JobStatus::Done, Some("P")),
                    job(8, JobStatus::Running, None),
                ],
            )
            .is_empty()
        );
    }

    #[test]
    fn a_new_job_starts() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[]);

        let events = observe(&tracker, &[job(7, JobStatus::Queued, None)]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].job_id, 7);
        assert_eq!(events[0].transition, Transition::Started);
        assert_eq!(events[0].workspace, WORKSPACE);
    }

    #[test]
    fn leaving_the_queue_does_not_start_a_second_time() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[]);
        assert_eq!(
            observe(&tracker, &[job(7, JobStatus::Queued, None)]).len(),
            1
        );

        assert!(observe(&tracker, &[job(7, JobStatus::Running, None)]).is_empty());
        assert!(observe(&tracker, &[job(7, JobStatus::Running, None)]).is_empty());
    }

    #[test]
    fn every_agent_chosen_at_once_starts() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[]);

        // The picker enqueues one job per chosen agent, so they all appear in the same pass.
        let events = observe(
            &tracker,
            &[
                job(9, JobStatus::Queued, None),
                job(8, JobStatus::Queued, None),
                job(7, JobStatus::Queued, None),
            ],
        );

        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.transition == Transition::Started)
        );
    }

    #[test]
    fn an_active_job_finishes() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[job(7, JobStatus::Running, None)]);

        let events = observe(&tracker, &[job(7, JobStatus::Done, Some("P"))]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].transition, Transition::Finished);
        assert_eq!(events[0].status, JobStatus::Done);
        assert_eq!(events[0].verdict.as_deref(), Some("P"));
    }

    #[test]
    fn a_finished_job_does_not_finish_twice() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[job(7, JobStatus::Running, None)]);
        assert_eq!(
            observe(&tracker, &[job(7, JobStatus::Done, Some("P"))]).len(),
            1
        );

        assert!(observe(&tracker, &[job(7, JobStatus::Done, Some("P"))]).is_empty());
    }

    #[test]
    fn a_review_shorter_than_the_poll_interval_still_finishes() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[]);

        // Enqueued and reviewed between two passes, so it is never observed queued or running.
        let events = observe(&tracker, &[job(7, JobStatus::Done, Some("P"))]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].transition, Transition::Finished);
        assert!(!events[0].errored());
        assert!(!events[0].has_findings());
    }

    #[test]
    fn two_workspaces_on_one_branch_report_a_transition_once() {
        let tracker = TransitionTracker::default();
        let checkout = Path::new(CHECKOUT);
        let running = [job(7, JobStatus::Running, None)];
        let done = [job(7, JobStatus::Done, Some("P"))];

        // Both workspaces are reconciled in the same pass, so they share a sequence.
        assert!(
            tracker
                .observe(checkout, BRANCH, WORKSPACE, 1, &running)
                .is_empty()
        );
        assert!(
            tracker
                .observe(checkout, BRANCH, "brewstack-wt", 1, &running)
                .is_empty()
        );

        let first = tracker.observe(checkout, BRANCH, WORKSPACE, 2, &done);
        let second = tracker.observe(checkout, BRANCH, "brewstack-wt", 2, &done);

        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[test]
    fn a_checkout_seen_for_the_first_time_primes_on_its_own() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[]);

        // The representative pane moved to another repo, whose history is not this one's news.
        let other = tracker.observe(
            Path::new("/other"),
            BRANCH,
            WORKSPACE,
            1,
            &[job(1, JobStatus::Done, Some("F"))],
        );

        assert!(other.is_empty());
    }

    #[test]
    fn switching_branches_primes_the_new_branch() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[job(7, JobStatus::Done, Some("P"))]);
        let checkout = Path::new(CHECKOUT);

        let historical = tracker.observe(
            checkout,
            "feature",
            WORKSPACE,
            1,
            &[job(8, JobStatus::Done, Some("F"))],
        );
        assert!(historical.is_empty());

        let events = tracker.observe(
            checkout,
            "feature",
            WORKSPACE,
            2,
            &[
                job(9, JobStatus::Done, Some("P")),
                job(8, JobStatus::Done, Some("F")),
            ],
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].job_id, 9);
        assert_eq!(events[0].transition, Transition::Finished);
    }

    #[test]
    fn a_job_that_ages_off_the_listing_is_forgotten() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[job(7, JobStatus::Running, None)]);

        assert!(observe(&tracker, &[]).is_empty());
    }

    #[test]
    fn a_pass_collapses_into_one_toast() {
        let errored = finished(WORKSPACE, 8, JobStatus::Failed, None);
        let findings = finished(WORKSPACE, 9, JobStatus::Done, Some("F"));
        // Only a Postgres sync pairs an errored status with a verdict.
        let synced = |id| finished(WORKSPACE, id, JobStatus::Failed, Some("F"));
        let passed = |id| finished(WORKSPACE, id, JobStatus::Done, Some("P"));

        let cases = [
            ("a quiet pass", vec![], None),
            (
                "one start keeps its detail",
                vec![started(WORKSPACE, 12)],
                Some(("roborev review started", "brewstack · job 12 · started")),
            ),
            (
                "one finish keeps its detail",
                vec![passed(12)],
                Some((
                    "roborev review finished",
                    "brewstack · job 12 · done · verdict P",
                )),
            ),
            (
                "one job seen from two worktrees counts once",
                vec![
                    passed(7),
                    finished("brewstack-wt", 7, JobStatus::Done, Some("P")),
                ],
                Some((
                    "roborev review finished",
                    "brewstack · job 7 · done · verdict P",
                )),
            ),
            (
                "a finished observation supersedes a start",
                vec![started("brewstack-wt", 7), passed(7)],
                Some((
                    "roborev review finished",
                    "brewstack · job 7 · done · verdict P",
                )),
            ),
            (
                "every agent chosen at once",
                vec![
                    started(WORKSPACE, 7),
                    started(WORKSPACE, 8),
                    started(WORKSPACE, 9),
                ],
                Some(("roborev reviews started", "brewstack · 3 started")),
            ),
            (
                "errors and findings counted apart",
                vec![passed(7), errored, findings],
                Some((
                    "roborev reviews finished",
                    "brewstack · 3 finished · 1 errored · 1 with findings",
                )),
            ),
            (
                "a verdict beside an errored status reports the review",
                vec![synced(8)],
                Some((
                    "roborev review finished",
                    "brewstack · job 8 · done · verdict F",
                )),
            ),
            (
                "a synced verdict counts as findings and never as an error",
                vec![passed(7), synced(8)],
                Some((
                    "roborev reviews finished",
                    "brewstack · 2 finished · 1 with findings",
                )),
            ),
            (
                "starts and finishes share a toast",
                vec![passed(7), started(WORKSPACE, 8)],
                Some(("roborev reviews", "brewstack · 1 started · 1 finished")),
            ),
            (
                "several workspaces are all named",
                vec![started(WORKSPACE, 7), started("mlbt-private", 8)],
                Some((
                    "roborev reviews started",
                    "2 started · brewstack, mlbt-private",
                )),
            ),
        ];

        for (name, events, want) in cases {
            let want = want.map(|(title, body)| (title, body.to_string()));
            assert_eq!(summarize(&events), want, "case: {name}");
        }
    }

    #[test]
    fn a_stale_snapshot_does_not_reopen_a_finished_job() {
        let tracker = TransitionTracker::default();
        let running = [job(7, JobStatus::Running, None)];
        let done = [job(7, JobStatus::Done, Some("P"))];
        let pass = |seq, jobs: &[ReviewJob]| {
            tracker.observe(Path::new(CHECKOUT), BRANCH, WORKSPACE, seq, jobs)
        };
        assert!(pass(1, &running).is_empty(), "priming pass");

        // Two workspaces share this checkout and branch, so they share one key. Each ran its own
        // `roborev list`, and the second workspace's snapshot predates the job finishing.
        let fresh = pass(2, &done);
        let stale = pass(2, &running);
        assert_eq!(fresh.len(), 1, "the fresh snapshot finishes the job");
        assert!(stale.is_empty(), "the stale snapshot restarts nothing");

        // The finish already went out, so seeing the job done again is not news.
        assert!(pass(3, &done).is_empty(), "finished twice");
    }

    #[test]
    fn a_rerun_under_the_original_job_id_starts_again() {
        let tracker = TransitionTracker::default();
        prime(&tracker, &[job(7, JobStatus::Done, Some("P"))]);

        // roborev's ReenqueueJob resets a rerun to queued under its original id, so terminal to
        // active is a real transition and cannot be used to recognise a stale snapshot.
        let events = observe(&tracker, &[job(7, JobStatus::Queued, None)]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].transition, Transition::Started);
    }

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
