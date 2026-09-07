use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::Result;

pub const PING: u8 = b'p';
pub const STOP: u8 = b's';
const READY: u8 = b'r';
const STOPPED: u8 = b'd';
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// How many exchanges a request makes before it reports that nothing answers. A reporter between
/// accept and reply closes the first one, and a socket nobody serves closes both.
const REQUEST_ATTEMPTS: u32 = 2;

/// Serve session control requests while the reporter holds its file lock.
pub struct Control {
    listener: UnixListener,
    path: PathBuf,
}

impl Control {
    /// Replace a stale socket after the caller acquires the session lock.
    pub fn bind(path: PathBuf) -> Result<Self> {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let control = Self { listener, path };
        fs::set_permissions(&control.path, fs::Permissions::from_mode(0o600))?;
        control.listener.set_nonblocking(true)?;
        Ok(control)
    }

    /// Drain queued requests before reconciliation and return a shutdown connection.
    pub fn poll(&self) -> Result<Option<UnixStream>> {
        loop {
            let (mut connection, _) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            connection.set_read_timeout(Some(READ_TIMEOUT))?;
            connection.set_write_timeout(Some(READ_TIMEOUT))?;
            let mut command = [0];
            if connection.read_exact(&mut command).is_ok() {
                match command[0] {
                    PING => {
                        let _ = connection.write_all(&[READY]);
                    }
                    STOP => return Ok(Some(connection)),
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acknowledge shutdown after releasing the child, socket, and lock.
pub fn reply_stopped(mut connection: UnixStream) {
    let _ = connection.write_all(&[STOPPED]);
}

/// Return false when nothing answers, and require a valid reply otherwise.
pub fn request(path: &Path, command: u8, timeout: Duration) -> Result<bool> {
    for _ in 0..REQUEST_ATTEMPTS {
        let Some(mut connection) = connect(path, timeout)? else {
            return Ok(false);
        };
        match exchange(&mut connection, command) {
            Ok(reply) => {
                let expected = if command == PING { READY } else { STOPPED };
                if reply != expected {
                    return Err(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "invalid reporter reply",
                    )
                    .into());
                }
                return Ok(true);
            }
            Err(err) if closed_before_reply(&err) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(false)
}

/// Send one command and read its single reply byte.
fn exchange(connection: &mut UnixStream, command: u8) -> std::io::Result<u8> {
    connection.write_all(&[command])?;
    let mut reply = [0];
    connection.read_exact(&mut reply)?;
    Ok(reply[0])
}

fn closed_before_reply(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
    )
}

/// Connect to a running reporter and apply the request deadline.
fn connect(path: &Path, timeout: Duration) -> Result<Option<UnixStream>> {
    let connection = match UnixStream::connect(path) {
        Ok(connection) => connection,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err.into()),
    };
    connection.set_read_timeout(Some(timeout))?;
    connection.set_write_timeout(Some(timeout))?;
    Ok(Some(connection))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::{Control, PING, READY, STOP, reply_stopped, request};

    #[test]
    fn ping_and_stop_require_acknowledgement() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("control.sock");
        let control = Control::bind(path.clone()).unwrap();
        let client_path = path.clone();
        let client = thread::spawn(move || {
            assert!(request(&client_path, PING, Duration::from_secs(2)).unwrap());
            assert!(request(&client_path, STOP, Duration::from_secs(2)).unwrap());
            assert!(!client_path.exists());
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline, "stop request did not arrive");
            if let Some(connection) = control.poll().unwrap() {
                drop(control);
                reply_stopped(connection);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        client.join().unwrap();
    }

    #[test]
    fn abandoned_and_invalid_clients_do_not_stop_the_reporter() {
        let dir = TempDir::new().unwrap();
        let control = Control::bind(dir.path().join("control.sock")).unwrap();
        let _silent = UnixStream::connect(&control.path).unwrap();
        assert!(control.poll().unwrap().is_none());
        let mut invalid = UnixStream::connect(&control.path).unwrap();
        invalid.write_all(b"?").unwrap();
        assert!(control.poll().unwrap().is_none());
    }

    #[test]
    fn stale_socket_is_absent_and_can_be_replaced() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("control.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(!request(&path, PING, Duration::from_secs(1)).unwrap());
        let _control = Control::bind(path).unwrap();
    }

    #[test]
    fn request_retries_a_connection_closed_before_its_reply() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            drop(listener.accept().unwrap());
            let (mut connection, _) = listener.accept().unwrap();
            let mut command = [0];
            connection.read_exact(&mut command).unwrap();
            assert_eq!(command, [PING]);
            connection.write_all(&[READY]).unwrap();
        });
        assert!(request(&path, PING, Duration::from_secs(1)).unwrap());
        server.join().unwrap();
    }
}
