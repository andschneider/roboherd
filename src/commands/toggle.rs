use std::time::Duration;

use crate::context::Context;
use crate::error::Result;
use crate::herdr;

/// How long the pane listing gets. This runs on a keypress, so it fails fast rather than hanging.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Bring the workspace's pane titled `label` forward, closing it only when it already has focus.
/// Reports whether a pane was there at all.
///
/// The pane listing avoids stale local state after a pane closes. Callers open their pane when this
/// returns `false`.
///
/// `label` is the manifest `title`, so pane titles in `herdr-plugin.toml` must remain distinct.
pub fn focus_or_close(context: &Context, label: &str) -> Result<bool> {
    match locate(context, label)? {
        Located::Missing => Ok(false),
        // Closing a pane in a tab nobody is looking at reads as the key doing nothing, so an open
        // pane elsewhere is brought forward instead. Pressing again, now on it, closes it.
        Located::Elsewhere(pane) => {
            herdr::focus_pane(&pane.pane_id)?;
            Ok(true)
        }
        Located::Focused(pane) => {
            herdr::close_pane(&pane.pane_id)?;
            Ok(true)
        }
    }
}

/// Where the workspace's pane titled `label` is, if anywhere.
pub enum Located {
    Missing,
    /// Open and holding focus, so the entrypoint that owns it is what you are looking at.
    Focused(herdr::Pane),
    /// Open in another tab or beside another focused pane.
    Elsewhere(herdr::Pane),
}

/// Find the workspace's pane titled `label`.
pub fn locate(context: &Context, label: &str) -> Result<Located> {
    let Some(workspace_id) = context.workspace_id.as_deref() else {
        return Ok(Located::Missing);
    };

    let found = herdr::pane_list(workspace_id, LIST_TIMEOUT)?
        .into_iter()
        .find(|pane| pane.label.as_deref() == Some(label));

    Ok(match found {
        None => Located::Missing,
        Some(pane) if pane.focused => Located::Focused(pane),
        Some(pane) => Located::Elsewhere(pane),
    })
}
