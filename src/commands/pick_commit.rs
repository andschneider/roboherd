use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::git;
use crate::panes::commit_picker::{Action, Picker};
use crate::roborev;
use crate::roborev::Selection;
use crate::tui;
use crate::tui::Screen;
use crate::wake;

/// How many commits the picker lists.
pub const DEFAULT_LIMIT: usize = 50;

/// How long the branch lookup behind the footer label may take.
const BRANCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the transient commit picker and enqueue its selection.
pub fn run(limit: usize) -> Result<()> {
    let context = Context::from_env()?;
    let checkout = context.existing_checkout()?;
    let commits = git::recent_commits(&checkout, limit)?;
    let dirty = git::dirty_count(&checkout)?;
    let branch = git::current_branch(&checkout, BRANCH_TIMEOUT)?;
    let installed = roborev::installed_agents();
    let default_agent = roborev::default_agent(&checkout);

    let mut screen = Screen::open()?;
    let mut picker = Picker::new(commits, dirty, branch, installed, default_agent, unix_now());

    let selection = loop {
        screen.draw(|frame| picker.render(frame))?;
        match picker.on_key(tui::next_key()?) {
            Action::Handled => {}
            Action::Quit => return Ok(()),
            Action::Enqueue => {
                let Some(selection) = picker.selection() else {
                    continue;
                };
                // A refusal leaves the picker where it was so the range can be adjusted and retried.
                match refusal(&checkout, &picker)? {
                    Some(reason) => picker.refuse(reason),
                    None => break selection,
                }
            }
        }
    };

    let agents = picker.take_agents();
    let review_type = picker.review_type();
    let mut replies = Vec::with_capacity(agents.len());
    let mut enqueued_any = false;

    for (done, agent) in agents.iter().enumerate() {
        // Draw progress before the daemon round-trip so the assigned job id remains visible.
        picker.set_enqueuing(done, agents.len());
        screen.draw(|frame| picker.render(frame))?;

        replies.push(
            match roborev::review(&checkout, &selection, review_type, agent.as_deref()) {
                Ok(reply) if reply.is_empty() => {
                    enqueued_any = true;
                    "queued".to_string()
                }
                Ok(reply) => {
                    enqueued_any = true;
                    reply
                }
                // One failed agent does not cancel the remaining enqueues.
                Err(err) => format!("{}: {err}", agent.as_deref().unwrap_or("default")),
            },
        );
    }
    if enqueued_any && let Err(err) = wake::touch() {
        eprintln!("roboherd: reporter wake failed: {err}");
    }
    picker.set_reported(replies);

    screen.draw(|frame| picker.render(frame))?;
    tui::next_key()?;
    Ok(())
}

/// Why the marked selection cannot be enqueued, or `None` when it can.
///
/// Picker rows use date order across branches, while `START^..END` uses ancestry. A dated
/// side-branch commit can appear inside the highlighted rows without belonging to the resolved
/// range.
pub(crate) fn refusal(checkout: &Path, picker: &Picker) -> Result<Option<String>> {
    let Some(Selection::Range(start, end)) = picker.selection() else {
        return Ok(None);
    };

    let resolved = match git::range_commits(checkout, &start, &end) {
        Ok(resolved) => resolved,
        // A root has no `START^`. Other failures still surface as Git errors.
        Err(err @ Error::CommandFailed { .. }) => {
            return match git::is_root_commit(checkout, &start) {
                Ok(true) => Ok(Some(
                    "the root commit cannot begin a range, it has no parent".to_string(),
                )),
                _ => Err(err),
            };
        }
        Err(other) => return Err(other),
    };

    let resolved: HashSet<&str> = resolved.iter().map(String::as_str).collect();
    let marked = picker.marked_shas();
    let skipped = marked
        .iter()
        .filter(|sha| !resolved.contains(**sha))
        .count();
    let extra = resolved.len() - (marked.len() - skipped);
    if extra == 0 && skipped == 0 {
        return Ok(None);
    }

    Ok(Some(format!(
        "range covers {extra} unmarked and skips {skipped} marked, branches interleave by date"
    )))
}

/// The current time as a unix timestamp, or zero when the clock predates the epoch.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}
