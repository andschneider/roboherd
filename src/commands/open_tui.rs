use crate::commands::toggle;
use crate::commands::toggle::Located;
use crate::config::{Config, TuiPlacement};
use crate::context::Context;
use crate::error::Result;
use crate::herdr;
use crate::roborev;

/// Entrypoint id of the pane that hosts the roborev TUI, in either placement.
pub const ENTRYPOINT: &str = "tui";

/// Manifest title of that pane, which is how a pane listing labels it.
pub const LABEL: &str = "roborev";

/// What the tab is called once it exists.
const TAB_TITLE: &str = "roborev";

/// Open the roborev TUI where `tui_placement` asks for it.
pub fn run() -> Result<()> {
    match Config::load()?.tui_placement {
        TuiPlacement::Popup => open_popup(),
        TuiPlacement::Tab => open_tab(),
    }
}

/// A popup targets the active pane, so it takes no target of its own. It never reaches a pane
/// listing, so there is nothing to toggle against and it only ever opens.
fn open_popup() -> Result<()> {
    herdr::open_pane(ENTRYPOINT, None, &[])
}

/// A tab is an ordinary pane, so a second invocation focuses or closes the open one.
fn open_tab() -> Result<()> {
    let context = Context::from_env()?;
    if toggle::focus_or_close(&context, LABEL)? {
        return Ok(());
    }

    herdr::open_pane_in_tab(ENTRYPOINT)?;
    name_tab(&context);
    Ok(())
}

/// Name the newly opened tab. A failure leaves the cosmetic default unchanged.
fn name_tab(context: &Context) {
    let found = match toggle::locate(context, LABEL) {
        Ok(Located::Focused(pane) | Located::Elsewhere(pane)) => pane,
        Ok(Located::Missing) => {
            eprintln!("roboherd: opened the roborev tab but could not find it to name");
            return;
        }
        Err(err) => {
            eprintln!("roboherd: opened the roborev tab but could not name it: {err}");
            return;
        }
    };

    if let Err(err) = herdr::rename_tab(&found.tab_id, TAB_TITLE) {
        eprintln!("roboherd: opened the roborev tab but could not name it: {err}");
    }
}

/// Run the roborev TUI inside the pane herdr opened. The TUI owns job browsing, findings, and
/// close, cancel, and rerun operations.
pub fn run_tui() -> Result<()> {
    let context = Context::from_env()?;
    roborev::tui(&context.existing_checkout()?)
}
