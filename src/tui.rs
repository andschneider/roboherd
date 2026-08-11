use std::io::stdout;
use std::ops::{Deref, DerefMut};
use std::panic::PanicHookInfo;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event;
use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyEvent, KeyEventKind,
};
use ratatui::crossterm::execute;

use crate::error::Result;

type PanicHook = Arc<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// Owns the terminal for the lifetime of a full-screen view.
///
/// A pane command that returns early through `?` still has to leave raw mode and the alternate
/// screen behind, so teardown hangs off `Drop` rather than the happy path. `try_init` also installs
/// a panic hook that restores the terminal first, so a panic mid-render does not leave the pane
/// wedged in raw mode.
pub struct Screen {
    terminal: DefaultTerminal,
    previous_panic_hook: PanicHook,
}

impl Screen {
    /// Enter raw mode and the alternate screen.
    pub fn open() -> Result<Self> {
        let terminal = ratatui::try_init()?;
        if let Err(err) = execute!(stdout(), EnableBracketedPaste) {
            ratatui::restore();
            return Err(err.into());
        }
        let previous_panic_hook = set_paste_panic_hook();
        Ok(Self {
            terminal,
            previous_panic_hook,
        })
    }
}

impl Deref for Screen {
    type Target = DefaultTerminal;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for Screen {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableBracketedPaste);
        ratatui::restore();
        if !std::thread::panicking() {
            let hook = Arc::clone(&self.previous_panic_hook);
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
    }
}

/// Install bracketed-paste cleanup and return the displaced hook.
fn set_paste_panic_hook() -> PanicHook {
    let hook = PanicHook::from(std::panic::take_hook());
    let chained = Arc::clone(&hook);
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste);
        chained(info);
    }));
    hook
}

/// Block until a pressed key, paste, or resize arrives.
pub fn next_event() -> Result<Event> {
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => return Ok(Event::Key(key)),
            event @ (Event::Paste(_) | Event::Resize(_, _)) => return Ok(event),
            _ => {}
        }
    }
}

/// Block until a pressed key, paste, or resize arrives, or `timeout` passes with none.
///
/// Ignored events do not restart the clock, so a terminal chattering key releases cannot hold the
/// deadline off forever.
pub fn next_event_within(timeout: Duration) -> Result<Option<Event>> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !event::poll(remaining)? {
            return Ok(None);
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => return Ok(Some(Event::Key(key))),
            event @ (Event::Paste(_) | Event::Resize(_, _)) => return Ok(Some(event)),
            _ => {}
        }
    }
}

/// Block until a key is pressed.
///
/// Key releases and repeats arrive as their own events under terminals that speak the kitty
/// protocol, so a plain `event::read` would act on one keystroke more than once.
pub fn next_key() -> Result<KeyEvent> {
    loop {
        if let Event::Key(key) = next_event()? {
            return Ok(key);
        }
    }
}
