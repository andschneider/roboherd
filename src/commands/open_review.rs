use std::time::Duration;

use crate::commands::toggle;
use crate::context;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::herdr;
use crate::roborev;

/// Entrypoint id of the review pane.
pub const ENTRYPOINT: &str = "review";

/// Manifest title of that pane, which is how a pane listing labels it. A listing carries no plugin
/// ownership, so the title is namespaced to keep the toggle off another plugin's pane.
pub const LABEL: &str = "roborev review";

/// Environment variable naming the job the review pane should render.
pub const JOB_ENV: &str = "ROBOHERD_JOB_ID";

/// How long the job lookup gets. This runs on a keypress, so it fails fast rather than hanging.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// How many jobs the newest-review lookup reads. Only the newest reviewed one is wanted, so a
/// short window is enough.
const LOOKUP_JOBS: usize = 50;

/// Toggle a pane showing the newest review worth reading.
pub fn run() -> Result<()> {
    let context = Context::from_env()?;
    if toggle::focus_or_close(&context, LABEL)? {
        return Ok(());
    }

    let checkout = context.existing_checkout()?;
    let jobs = roborev::list(&checkout, None, LOOKUP_JOBS, LOOKUP_TIMEOUT)?;
    let job = roborev::newest_reviewed(&jobs).ok_or(Error::NoFinishedReview)?;

    herdr::open_pane(
        ENTRYPOINT,
        context::focused_pane_id(&context).as_deref(),
        &[(JOB_ENV, job.id.to_string())],
    )
}
