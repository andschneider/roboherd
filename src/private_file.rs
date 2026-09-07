//! Every file roboherd creates in a shared directory is opened here.

use std::fs::File;
use std::io;
use std::path::Path;

use rustix::fs::{FileType, Mode, OFlags, fstat, open};

/// Open a private file for writing without following a symlink planted at its path.
///
/// The reporter's fallback paths live in world-writable `/tmp`, so a squatter must not be able to
/// redirect a write into a file the reporter owns. `NOFOLLOW` refuses a symlink, `NONBLOCK` keeps a
/// planted FIFO from blocking the open, and the mode check rejects everything that is not a plain
/// file. Existing contents are left alone, so a caller replacing a document unlinks it first.
pub fn create(path: &Path) -> io::Result<File> {
    let fd = open(
        path,
        OFlags::CREATE | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::from_raw_mode(0o600),
    )?;
    let stat = fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(File::from(fd))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::create;

    #[test]
    fn a_new_file_is_private() {
        let dir = tempdir().expect("temporary directory");
        let path = dir.path().join("document");

        create(&path).expect("create");

        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_symlink_is_refused_without_touching_its_target() {
        let dir = tempdir().expect("temporary directory");
        let target = dir.path().join("target");
        let path = dir.path().join("document");
        fs::write(&target, "untouched").expect("target");
        symlink(&target, &path).expect("symlink");

        assert!(create(&path).is_err());
        assert_eq!(
            fs::read_to_string(target).expect("target contents"),
            "untouched"
        );
    }
}
