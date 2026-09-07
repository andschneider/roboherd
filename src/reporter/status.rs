use std::fs;
use std::io;
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private_file;

const MAX_STATUS_BYTES: usize = 4096;

/// The reporter facts cached for diagnostics.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReporterStatus {
    pub version: String,
    pub pid: u32,
    pub started_at: u64,
    pub last_pass_at: Option<u64>,
    /// Absent on a status file from a not-yet-restarted, pre-upgrade reporter.
    #[serde(default)]
    pub errors: Vec<String>,
    pub stream_error: Option<String>,
}

/// Own the current reporter's atomic status file.
pub struct Store {
    path: PathBuf,
    temporary: PathBuf,
}

impl Store {
    /// Remove stale state before publishing this reporter's first snapshot.
    pub fn new(path: PathBuf) -> io::Result<Self> {
        let temporary = path.with_extension("status.tmp");
        remove_if_present(&path)?;
        remove_if_present(&temporary)?;
        Ok(Self { path, temporary })
    }

    /// Replace the visible snapshot after writing the complete document.
    ///
    /// The temporary is removed rather than truncated so each update lands on a file this reporter
    /// created, and a squatter that wins the gap between the two is refused rather than followed.
    pub fn write(&self, status: &ReporterStatus) -> io::Result<()> {
        let bytes = serde_json::to_vec(status).map_err(io::Error::other)?;
        remove_if_present(&self.temporary)?;
        let mut file = private_file::create(&self.temporary)?;
        file.write_all(&bytes)?;
        fs::rename(&self.temporary, &self.path)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.temporary);
    }
}

/// Read the reporter snapshot when one has been published.
pub fn read(path: &Path) -> io::Result<Option<ReporterStatus>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if bytes.len() > MAX_STATUS_BYTES {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "oversized reporter status",
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(io::Error::other)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{ReporterStatus, Store, read};

    #[test]
    fn store_replaces_and_removes_the_status_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reporter.status");
        let store = Store::new(path.clone()).unwrap();
        let status = ReporterStatus {
            version: "test".to_string(),
            pid: 42,
            started_at: 1,
            last_pass_at: Some(2),
            errors: Vec::new(),
            stream_error: Some("offline".to_string()),
        };
        store.write(&status).unwrap();
        let stored = read(&path).unwrap().unwrap();
        assert_eq!(stored.pid, 42);
        assert_eq!(stored.stream_error.as_deref(), Some("offline"));
        drop(store);
        assert!(read(&path).unwrap().is_none());
    }
}
