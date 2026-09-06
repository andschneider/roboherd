use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::badge::Badge;
use crate::error::Result;
use crate::git;
use crate::herdr;
use crate::reporter::control;
use crate::reporter::lock;
use crate::reporter::schedule::{self, POLL_INTERVAL, WAKE_TICK};
use crate::reporter::startup::Startup;
use crate::reporter::status;
use crate::reporter::status::ReporterStatus;
use crate::reporter::stream;
use crate::reporter::transitions::{Event, TransitionTracker, summarize};
use crate::roborev;
use crate::wake::{self, Marker};

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
    /// Workspace failures retained for status after they are logged.
    errors: Mutex<Vec<String>>,
    /// The pass's panes, grouped by workspace. Taken from the same snapshot as the workspaces.
    panes: HashMap<String, Vec<herdr::Pane>>,
}

/// Run the reconcile loop until the process is killed. One reporter serves every workspace, and a
/// failure in one workspace never ends the loop.
///
/// A second reporter would race metadata writes and make badges alternate between snapshots.
/// `--once` skips the lock because it performs one diagnostic pass.
pub fn run(once: bool, verbose: bool, mut startup: Startup) -> Result<()> {
    // Held for the whole run. Dropping it early would let a second reporter in mid-loop.
    let reporter_lock = if once {
        None
    } else {
        Some(lock::claim(COMMAND_TIMEOUT)?)
    };
    let tracker = TransitionTracker::default();

    if once {
        if let Err(err) = reconcile_all(verbose, &tracker) {
            eprintln!("roboherd: snapshot failed: {err}");
        }
        return Ok(());
    }

    // Started after the lock, so only the reporter that won it holds a stream child.
    let control = control::Control::bind(lock::lock_path().with_extension("sock"))?;
    let mut stream = stream::Watcher::default();
    stream.tick();
    let mut runtime = ReporterStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        started_at: unix_seconds(),
        last_pass_at: None,
        last_error: None,
        stream_error: stream.error(),
    };
    let status_store = status::Store::new(lock::lock_path().with_extension("status"))?;
    publish_status(&status_store, &runtime);

    let mut next_full_pass = Instant::now();
    let mut last_wake = schedule::observed_time(wake::observe());

    loop {
        if startup.cancelled()? {
            return Ok(());
        }
        stream.tick();
        let stream_error = stream.error();
        if runtime.stream_error != stream_error {
            runtime.stream_error = stream_error;
            publish_status(&status_store, &runtime);
        }
        if let Some(request) = control.poll()? {
            drop(stream);
            drop(control);
            drop(status_store);
            drop(reporter_lock);
            control::reply_stopped(request);
            return Ok(());
        }
        let started = Instant::now();
        let (run_pass, observed) = schedule::scheduling_decision(
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
            let error = match reconcile_all(verbose, &tracker) {
                Ok(error) => error,
                Err(err) => {
                    eprintln!("roboherd: snapshot failed: {err}");
                    Some(err.to_string())
                }
            };
            runtime.last_pass_at = Some(unix_seconds());
            runtime.last_error = error.map(|error| bounded_error(&error));
            publish_status(&status_store, &runtime);
            next_full_pass = started + POLL_INTERVAL;
        }
        thread::sleep(WAKE_TICK);
    }
}

/// Limit errors written to the local status file.
fn bounded_error(error: &str) -> String {
    error.chars().take(512).collect()
}

fn publish_status(store: &status::Store, status: &ReporterStatus) {
    if let Err(err) = store.write(status) {
        eprintln!("roboherd: status update failed: {err}");
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Reconcile every open workspace in one pass.
fn reconcile_all(verbose: bool, tracker: &TransitionTracker) -> Result<Option<String>> {
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
        errors: Mutex::new(Vec::new()),
        panes: herdr::panes_by_workspace(panes),
    };

    // Reconcile workspaces concurrently. Each one spawns at least one child process and waits on
    // the roborev daemon, so a serial pass over several workspaces can outlast the token TTL.
    thread::scope(|scope| {
        for workspace in &workspaces {
            let pass = &pass;
            scope.spawn(move || {
                if let Err(err) = reconcile(workspace, pass) {
                    let error = format!(
                        "workspace {} ({}) failed: {err}",
                        workspace.workspace_id, workspace.label
                    );
                    eprintln!("roboherd: {error}");
                    pass.errors
                        .lock()
                        .expect("pass errors poisoned")
                        .push(error);
                }
            });
        }
    });

    // Raised after the scope joins so notification latency cannot delay this pass's metadata writes.
    notify_pass(
        pass.events.into_inner().expect("pass events poisoned"),
        verbose,
    );
    let errors = pass.errors.into_inner().expect("pass errors poisoned");
    Ok(match errors.as_slice() {
        [] => None,
        [error] => Some(error.clone()),
        [first, rest @ ..] => Some(format!("{first} (+{} more workspace failures)", rest.len())),
    })
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
