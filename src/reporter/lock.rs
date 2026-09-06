use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::process::getuid;

use crate::error::{Error, Result};
use crate::herdr;

/// Herdr's env var naming this session's control socket. Each named session is its own server, so
/// this also names a directory unique to it.
const SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";

/// The file whose lock marks the running reporter for one herdr session.
///
/// Scoped to the session's socket directory rather than the uid, so two sessions each get their
/// own reporter instead of one starving the other. Falls back to the uid-scoped `/tmp` path when
/// run outside a herdr-launched context.
pub fn lock_path() -> PathBuf {
    if let Some(dir) = socket_dir(std::env::var(SOCKET_PATH_ENV).ok().as_deref()) {
        return dir.join("roboherd-reporter.lock");
    }
    let uid = getuid().as_raw();
    PathBuf::from("/tmp").join(format!("roboherd-reporter-{uid}.lock"))
}

/// The directory holding this session's control socket, when herdr set one.
///
/// Takes the raw value instead of reading the env var directly, so this is testable without
/// touching process-global state. `Path::parent` returns `Some("")` rather than `None` for a bare
/// relative filename, which would otherwise resolve the lock beside the reporter's cwd instead of
/// falling back to `/tmp`.
fn socket_dir(socket_path: Option<&str>) -> Option<PathBuf> {
    let socket_path = socket_path?.trim();
    if socket_path.is_empty() {
        return None;
    }
    let parent = Path::new(socket_path).parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.to_path_buf())
}

/// Take the session lock without changing any existing file contents.
pub fn claim(notify_timeout: Duration) -> Result<File> {
    match try_claim(&lock_path())? {
        Some(file) => Ok(file),
        None => {
            let _ = herdr::notify(
                "roboherd: reporter already running",
                "Another reporter is publishing review state. Stop it before starting another.",
                notify_timeout,
            );
            Err(Error::ReporterAlreadyRunning)
        }
    }
}

/// Return the lock when no reporter holds it.
///
/// The mode is explicit because the CLI commands that claim the lock never set a creation mask.
pub fn try_claim(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(Error::Io(source)),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::socket_dir;

    #[test]
    fn socket_dir_names_the_directory_holding_the_session_socket() {
        assert_eq!(
            socket_dir(Some(
                "/Users/andrew/.config/herdr/sessions/session-2/herdr.sock"
            )),
            Some(PathBuf::from(
                "/Users/andrew/.config/herdr/sessions/session-2"
            ))
        );
    }

    #[test]
    fn socket_dir_is_absent_without_a_socket_path() {
        assert_eq!(socket_dir(None), None);
        assert_eq!(socket_dir(Some("")), None);
        assert_eq!(socket_dir(Some("   ")), None);
    }

    #[test]
    fn socket_dir_is_absent_for_a_bare_relative_filename() {
        assert_eq!(socket_dir(Some("socket")), None);
    }
}
