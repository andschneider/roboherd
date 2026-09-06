use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::roborev;

/// What a job did between two passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Started,
    Finished,
}

/// One notifiable job transition, tagged with the workspace it was seen from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The workspace label, so a toast can name where the review is.
    pub workspace: String,
    pub job_id: i64,
    pub transition: Transition,
    pub status: roborev::JobStatus,
    pub verdict: Option<String>,
}

/// Job statuses from the previous pass, keyed by shared Git directory and branch.
///
/// A newly observed branch is primed without reporting its existing history.
#[derive(Debug, Default)]
pub struct TransitionTracker {
    targets: Mutex<HashMap<(PathBuf, String), TargetState>>,
}

/// One query target's last-seen job statuses.
#[derive(Debug, Default)]
struct TargetState {
    /// False until this target has been polled once, so the first pass records and stays silent.
    ///
    /// Without it every reporter start would announce each review the daemon still remembers.
    primed: bool,
    /// The pass this key last accepted, which is what makes a second look at it in the same pass a
    /// duplicate rather than news.
    seq: u64,
    statuses: HashMap<i64, roborev::JobStatus>,
}

impl Event {
    fn is_preferred_over(&self, other: &Self) -> bool {
        match (self.transition, other.transition) {
            (Transition::Finished, Transition::Started) => true,
            (Transition::Started, Transition::Finished) => false,
            _ => self.workspace < other.workspace,
        }
    }

    /// Whether the review job failed without producing a verdict.
    ///
    /// A failing verdict is a completed review that found problems and reaches the sidebar badge.
    pub fn errored(&self) -> bool {
        self.transition == Transition::Finished
            && self.status == roborev::JobStatus::Failed
            && self.verdict.is_none()
    }

    /// Whether the completed review produced findings.
    pub fn has_findings(&self) -> bool {
        self.transition == Transition::Finished && self.verdict.as_deref() == Some("F")
    }

    /// This transition on its own, in the detail a single-event pass can afford.
    fn detail(&self) -> String {
        let Event {
            workspace, job_id, ..
        } = self;
        match self.transition {
            Transition::Started => format!("{workspace} · job {job_id} · started"),
            Transition::Finished => {
                // A verdict means a review exists to report, whatever the status beside it says.
                let status = if self.errored() { "errored" } else { "done" };
                let verdict = self
                    .verdict
                    .as_deref()
                    .map(|value| format!(" · verdict {value}"))
                    .unwrap_or_default();
                format!("{workspace} · job {job_id} · {status}{verdict}")
            }
        }
    }
}

impl TransitionTracker {
    /// Diff `jobs` against the last pass and record them as the new baseline.
    ///
    /// Only the first snapshot for a repository and branch is accepted in each pass. A later
    /// workspace snapshot may be stale and would otherwise reverse a transition already observed.
    ///
    /// Status alone cannot identify staleness because a rerun moves its original job id back to
    /// `queued`.
    pub fn observe(
        &self,
        repository: &Path,
        branch: &str,
        workspace: &str,
        seq: u64,
        jobs: &[roborev::ReviewJob],
    ) -> Vec<Event> {
        let mut targets = self.targets.lock().expect("transition tracker poisoned");
        let state = targets
            .entry((repository.to_path_buf(), branch.to_string()))
            .or_default();
        if state.primed && state.seq == seq {
            return Vec::new();
        }
        state.seq = seq;

        let mut events = Vec::new();
        let mut statuses = HashMap::with_capacity(jobs.len());

        for job in jobs {
            let previous = state.statuses.get(&job.id).copied();
            statuses.insert(job.id, job.status);
            if !state.primed {
                continue;
            }

            // Queued and running count as one state, so a job leaving the queue starts once. An
            // unseen job that is already terminal finished inside one interval, never seen active.
            let started =
                job.status.is_active() && !previous.is_some_and(roborev::JobStatus::is_active);
            let finished =
                job.status.is_reviewed() && !previous.is_some_and(roborev::JobStatus::is_reviewed);

            let Some(transition) = started
                .then_some(Transition::Started)
                .or(finished.then_some(Transition::Finished))
            else {
                continue;
            };

            events.push(Event {
                workspace: workspace.to_string(),
                job_id: job.id,
                transition,
                status: job.status,
                verdict: job.verdict.clone(),
            });
        }

        // Replaced rather than merged, so jobs that age off roborev's listing age out of here too.
        state.statuses = statuses;
        state.primed = true;
        events
    }
}

/// Fold a pass's transitions into the one toast it gets, or nothing when the pass was quiet.
///
/// Herdr drops additional notifications as `busy`, so every workspace and transition shares one
/// summary.
pub fn summarize(events: &[Event]) -> Option<(&'static str, String)> {
    let mut by_job: HashMap<i64, &Event> = HashMap::with_capacity(events.len());
    for event in events {
        let current = by_job.entry(event.job_id).or_insert(event);
        if event.is_preferred_over(current) {
            *current = event;
        }
    }

    let mut events: Vec<&Event> = by_job.into_values().collect();
    events.sort_by(|a, b| a.workspace.cmp(&b.workspace).then(a.job_id.cmp(&b.job_id)));

    let [first, rest @ ..] = events.as_slice() else {
        return None;
    };
    if rest.is_empty() {
        let title = match first.transition {
            Transition::Started => "roborev review started",
            Transition::Finished => "roborev review finished",
        };
        return Some((title, first.detail()));
    }

    let tally = |matches: fn(&Event) -> bool| events.iter().filter(|event| matches(event)).count();
    let started = tally(|event| event.transition == Transition::Started);
    let finished = tally(|event| event.transition == Transition::Finished);
    let errored = tally(Event::errored);
    let findings = tally(Event::has_findings);

    let title = match (started > 0, finished > 0) {
        (true, false) => "roborev reviews started",
        (false, true) => "roborev reviews finished",
        _ => "roborev reviews",
    };

    let counts = [
        (started, "started"),
        (finished, "finished"),
        (errored, "errored"),
        (findings, "with findings"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect::<Vec<_>>()
    .join(" · ");

    // Sorted by workspace above, so repeats of one label are already adjacent.
    let mut labels: Vec<&str> = events
        .iter()
        .map(|event| event.workspace.as_str())
        .collect();
    labels.dedup();

    let body = match labels.as_slice() {
        [only] => format!("{only} · {counts}"),
        _ => format!("{counts} · {}", labels.join(", ")),
    };
    Some((title, body))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Event, Transition, TransitionTracker, summarize};
    use crate::roborev::{JobStatus, ReviewJob};

    const CHECKOUT: &str = "/repo";
    const BRANCH: &str = "main";
    const WORKSPACE: &str = "repo1";

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
                .observe(checkout, BRANCH, "repo1-wt", 1, &running)
                .is_empty()
        );

        let first = tracker.observe(checkout, BRANCH, WORKSPACE, 2, &done);
        let second = tracker.observe(checkout, BRANCH, "repo1-wt", 2, &done);

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
                Some(("roborev review started", "repo1 · job 12 · started")),
            ),
            (
                "one finish keeps its detail",
                vec![passed(12)],
                Some((
                    "roborev review finished",
                    "repo1 · job 12 · done · verdict P",
                )),
            ),
            (
                "one job seen from two worktrees counts once",
                vec![
                    passed(7),
                    finished("repo1-wt", 7, JobStatus::Done, Some("P")),
                ],
                Some((
                    "roborev review finished",
                    "repo1 · job 7 · done · verdict P",
                )),
            ),
            (
                "a finished observation supersedes a start",
                vec![started("repo1-wt", 7), passed(7)],
                Some((
                    "roborev review finished",
                    "repo1 · job 7 · done · verdict P",
                )),
            ),
            (
                "every agent chosen at once",
                vec![
                    started(WORKSPACE, 7),
                    started(WORKSPACE, 8),
                    started(WORKSPACE, 9),
                ],
                Some(("roborev reviews started", "repo1 · 3 started")),
            ),
            (
                "errors and findings counted apart",
                vec![passed(7), errored, findings],
                Some((
                    "roborev reviews finished",
                    "repo1 · 3 finished · 1 errored · 1 with findings",
                )),
            ),
            (
                "a verdict beside an errored status reports the review",
                vec![synced(8)],
                Some((
                    "roborev review finished",
                    "repo1 · job 8 · done · verdict F",
                )),
            ),
            (
                "a synced verdict counts as findings and never as an error",
                vec![passed(7), synced(8)],
                Some((
                    "roborev reviews finished",
                    "repo1 · 2 finished · 1 with findings",
                )),
            ),
            (
                "starts and finishes share a toast",
                vec![passed(7), started(WORKSPACE, 8)],
                Some(("roborev reviews", "repo1 · 1 started · 1 finished")),
            ),
            (
                "several workspaces are all named",
                vec![started(WORKSPACE, 7), started("repo2", 8)],
                Some(("roborev reviews started", "2 started · repo1, repo2")),
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
}
