use crate::error::Result;
use crate::herdr;

/// Entrypoint id of the transient commit picker pane.
pub const ENTRYPOINT: &str = "commit-picker";

/// Open the commit picker pane. A popup targets the active pane, so it takes no target of its own.
pub fn run() -> Result<()> {
    herdr::open_pane(ENTRYPOINT, None, &[])
}
