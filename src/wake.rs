use std::fs::{self, File, FileTimes};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rustix::fs::{FileType, Mode, OFlags, fstat, open};
use rustix::process::getuid;

/// What one look at the reporter wake marker found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// The marker's modification time.
    At(SystemTime),
    /// The marker does not exist yet.
    Absent,
    /// The marker could not be read safely.
    Unreadable,
}

/// Update the reporter marker after roborev state may have changed.
pub fn touch() -> io::Result<()> {
    touch_path(&path())
}

/// Observe the reporter marker without following symlinks.
pub fn observe() -> Marker {
    observe_path(&path())
}

/// The account-scoped reporter wake marker.
fn path() -> PathBuf {
    let uid = getuid().as_raw();
    PathBuf::from("/tmp").join(format!("roboherd-reporter-{uid}.wake"))
}

fn touch_path(path: &Path) -> io::Result<()> {
    let fd = open(
        path,
        OFlags::CREATE | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::from_raw_mode(0o600),
    )?;
    let stat = fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(io::Error::other(
            "reporter wake marker is not a regular file",
        ));
    }

    File::from(fd).set_times(FileTimes::new().set_modified(SystemTime::now()))
}

fn observe_path(path: &Path) -> Marker {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => match metadata.modified() {
            Ok(modified) => Marker::At(modified),
            Err(_) => Marker::Unreadable,
        },
        Ok(_) => Marker::Unreadable,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Marker::Absent,
        Err(_) => Marker::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::tempdir;

    use super::touch_path;

    #[test]
    fn touch_creates_a_private_regular_marker() {
        let dir = tempdir().expect("temporary directory");
        let marker = dir.path().join("wake");

        touch_path(&marker).expect("touch marker");

        assert!(marker.is_file());
        assert_eq!(
            fs::metadata(marker).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn touch_refuses_a_symlink() {
        let dir = tempdir().expect("temporary directory");
        let target = dir.path().join("target");
        let marker = dir.path().join("wake");
        fs::write(&target, "untouched").expect("target");
        symlink(&target, &marker).expect("symlink");

        assert!(touch_path(&marker).is_err());
        assert_eq!(
            fs::read_to_string(target).expect("target contents"),
            "untouched"
        );
    }
}
