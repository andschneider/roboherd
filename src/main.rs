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
mod reporter;
mod roborev;
mod tui;
mod wake;

use clap::Parser;

use crate::cli::Cli;

/// Manifest id of the plugin this binary backs. Pane-open requests are scoped by it.
pub const PLUGIN_ID: &str = "roboherd";

fn main() {
    if let Err(err) = Cli::parse().run() {
        eprintln!("roboherd: {err}");
        std::process::exit(1);
    }
}
