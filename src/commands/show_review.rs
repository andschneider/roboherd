use std::path::Path;
use std::time::Duration;

use ratatui::crossterm::event::Event;

use crate::clipboard;
use crate::commands::open_review::JOB_ENV;
use crate::context::Context;
use crate::error::Result;
use crate::panes::review::{Action, ReviewView};
use crate::roborev;
use crate::tui;
use crate::tui::Screen;
use crate::wake;

/// Maximum time for one interactive roborev command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How many jobs stepping reads. The listing is per-branch, so this bounds it by what anyone steps
/// through by hand rather than by a branch's history.
const STEP_JOBS: usize = 200;

/// How long a footer notice stays before the key hints have the line to themselves again.
const NOTICE_TTL: Duration = Duration::from_secs(2);

/// Render and act on one review inside its pane.
pub fn run() -> Result<()> {
    let context = Context::from_env()?;
    let checkout = context.existing_checkout()?;
    let job_id = job_id();
    let review = fetch_review(&checkout, job_id)?;
    let jobs = steppable_jobs(&checkout).unwrap_or_default();

    let mut screen = Screen::open()?;
    let mut view = ReviewView::from_review(job_id, jobs, &review);

    loop {
        screen.draw(|frame| view.render(frame))?;

        // A notice is transient, so waiting on it expires it rather than pinning it until a keypress.
        let event = match view.has_notice() {
            false => tui::next_event()?,
            true => match tui::next_event_within(NOTICE_TTL)? {
                Some(event) => event,
                None => {
                    view.clear_notice();
                    continue;
                }
            },
        };

        let action = match event {
            Event::Key(key) => view.on_key(key),
            Event::Paste(text) => view.on_paste(&text),
            Event::Resize(_, _) => continue,
            _ => continue,
        };

        match action {
            Action::Handled => {}
            Action::Quit => return Ok(()),
            Action::Refresh => refresh_review(&checkout, &mut view),
            Action::Close => close_review(&checkout, &mut view),
            Action::SubmitComment(message) => submit_comment(&checkout, &mut view, &message),
            Action::Show(job_id) => show_job(&checkout, &mut view, job_id),
            Action::Copy => copy_review(&mut view),
        }
    }
}

/// Jobs the pane can step through, newest first, or `None` when the listing failed.
///
/// A failure is distinct from an empty list: it leaves whatever the pane already had, rather than
/// silently ending stepping.
fn steppable_jobs(checkout: &Path) -> Option<Vec<i64>> {
    let jobs = roborev::list(checkout, None, STEP_JOBS, COMMAND_TIMEOUT).ok()?;
    Some(
        jobs.iter()
            .filter(|job| job.reached_verdict())
            .map(|job| job.id)
            .collect(),
    )
}

/// Fetch another job's review, leaving the displayed one in place when it cannot be read.
fn show_job(checkout: &Path, view: &mut ReviewView, job_id: i64) {
    match fetch_review(checkout, Some(job_id)) {
        Ok(review) => view.show_job(job_id, &review),
        Err(err) => view.set_notice(format!("job {job_id} failed to load: {err}")),
    }
}

/// Reload the displayed job and the jobs it can step to, and return to the top.
fn refresh_review(checkout: &Path, view: &mut ReviewView) {
    match fetch_review(checkout, view.job_id()) {
        Ok(review) => {
            if let Some(jobs) = steppable_jobs(checkout) {
                view.set_jobs(jobs);
            }
            view.replace_review(&review, "review refreshed".to_string());
        }
        Err(err) => view.set_notice(format!("refresh failed: {err}")),
    }
}

/// Close the displayed job and wake the reporter best-effort.
fn close_review(checkout: &Path, view: &mut ReviewView) {
    let Some(job_id) = view.job_id() else {
        return;
    };

    match roborev::close(checkout, job_id, COMMAND_TIMEOUT) {
        Ok(()) => {
            let notice = match wake::touch() {
                Ok(()) => "review closed".to_string(),
                Err(err) => format!("review closed; reporter wake failed: {err}"),
            };
            view.mark_closed(notice);
        }
        Err(err) => view.set_notice(format!("close failed: {err}")),
    }
}

/// Submit a comment and refresh the review after it is saved.
fn submit_comment(checkout: &Path, view: &mut ReviewView, message: &str) {
    let Some(job_id) = view.job_id() else {
        return;
    };

    if let Err(err) = roborev::comment(checkout, job_id, message, COMMAND_TIMEOUT) {
        view.set_notice(format!("comment failed: {err}"));
        return;
    }

    view.finish_comment();
    match fetch_review(checkout, Some(job_id)) {
        Ok(review) => view.replace_review(&review, "comment added".to_string()),
        Err(err) => view.set_notice(format!("comment added; refresh failed: {err}")),
    }
}

/// Copy the displayed review text to the clipboard.
fn copy_review(view: &mut ReviewView) {
    match clipboard::copy(view.review_text()) {
        Ok(()) => view.set_notice("review copied".to_string()),
        Err(err) => view.set_notice(format!("copy failed: {err}")),
    }
}

fn fetch_review(checkout: &Path, job_id: Option<i64>) -> Result<roborev::ShownReview> {
    roborev::show(checkout, job_id, COMMAND_TIMEOUT)
}

fn job_id() -> Option<i64> {
    std::env::var(JOB_ENV).ok()?.trim().parse().ok()
}
