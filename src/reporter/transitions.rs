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
