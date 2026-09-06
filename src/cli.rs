use clap::{Parser, Subcommand};

use crate::commands;
use crate::error::Result;

/// One binary backs every plugin entrypoint. Each subcommand maps to one manifest startup hook,
/// pane, or action.
#[derive(Debug, Parser)]
#[command(name = "roboherd", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check the reporter serving the current herdr session
    Doctor,
    /// Poll roborev for every open workspace and publish the $roborev sidebar token
    Reporter {
        /// Reconcile once and exit instead of looping
        #[arg(long)]
        once: bool,
        /// Log what each workspace resolved to
        #[arg(long, short)]
        verbose: bool,
        /// Require the spawning CLI to confirm startup through stdin
        #[arg(long, hide = true, conflicts_with = "once")]
        startup_handshake: bool,
    },
    /// Start this session's reporter in the background
    StartReporter,
    /// Stop this session's reporter, if one is running
    StopReporter,
    /// Ask the reporter to reconcile after roborev state changes
    Wake,
    /// Open the commit picker pane for the active workspace
    OpenPicker,
    /// List uncommitted changes and recent commits, and enqueue a review for the selection
    PickCommit {
        /// How many commits to list
        #[arg(long, default_value_t = commands::pick_commit::DEFAULT_LIMIT)]
        limit: usize,
    },
    /// Toggle a pane for reading, closing, or commenting on the newest finished review
    OpenReview,
    /// Render a review inside the pane opened for it
    ShowReview,
    /// Open the roborev TUI for the active workspace, as a popup or a tab
    OpenTui,
    /// Run the roborev TUI scoped to the active repo and branch
    Tui,
}

impl Cli {
    /// Dispatch the parsed subcommand.
    pub fn run(self) -> Result<()> {
        match self.command {
            Command::Doctor => commands::doctor::run(),
            Command::Reporter {
                once,
                verbose,
                startup_handshake,
            } => commands::reporter::run(once, verbose, startup_handshake),
            Command::StartReporter => commands::reporter::start(),
            Command::StopReporter => commands::reporter::stop(),
            Command::Wake => wake(),
            Command::OpenPicker => commands::open_picker::run(),
            Command::PickCommit { limit } => commands::pick_commit::run(limit),
            Command::OpenReview => commands::open_review::run(),
            Command::ShowReview => commands::show_review::run(),
            Command::OpenTui => commands::open_tui::run(),
            Command::Tui => commands::open_tui::run_tui(),
        }
    }
}

fn wake() -> Result<()> {
    crate::wake::touch()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
