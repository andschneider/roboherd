mod agents;
mod badge;
mod cli;
mod clipboard;
mod commands;
mod config;
mod context;
mod error;
mod exec;
mod git;
mod herdr;
mod panes;
mod private_file;
mod reporter;
mod requirements;
mod roborev;
mod tui;
mod wake;

use std::time::Duration;

use clap::Parser;

use crate::cli::Cli;

/// Manifest id of the plugin this binary backs. Pane-open requests are scoped by it.
pub const PLUGIN_ID: &str = "roboherd";

/// How long the failure toast gets, short because the process is already on its way out.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

fn main() {
    if let Err(err) = Cli::parse().run() {
        eprintln!("roboherd: {err}");

        // An action's stderr reaches only herdr's plugin log, so a failure is otherwise silent.
        let _ = herdr::notify("roboherd", &err.summary(), NOTIFY_TIMEOUT);
        std::process::exit(1);
    }
}
