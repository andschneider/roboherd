use crate::error::Result;

/// Run the workspace reporter.
pub fn run(once: bool, verbose: bool) -> Result<()> {
    crate::reporter::poller::run(once, verbose)
}
