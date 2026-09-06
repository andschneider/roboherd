//! Reporter polling and transitions live here. CLI dispatch belongs in commands.

pub(crate) mod control;
pub(crate) mod lock;
pub mod poller;
mod schedule;
pub(crate) mod startup;
mod stream;
mod transitions;
