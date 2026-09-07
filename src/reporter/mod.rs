//! Reporter polling and transitions live here. CLI dispatch belongs in commands.

pub(crate) mod control;
pub(crate) mod lock;
pub mod poller;
pub(crate) mod schedule;
pub(crate) mod startup;
pub(crate) mod status;
mod stream;
mod transitions;
