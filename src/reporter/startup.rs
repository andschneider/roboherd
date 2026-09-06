use std::fs::File;
use std::io;
use std::io::Read;
use std::os::fd::AsFd;

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

pub const CONFIRMED: u8 = b'r';

/// Cancel an unconfirmed reporter when its spawning CLI closes the private pipe.
#[derive(Default)]
pub struct Startup {
    pipe: Option<File>,
}

impl Startup {
    /// Watch piped stdin only for reporters spawned by the start command.
    pub fn from_stdin(enabled: bool) -> io::Result<Self> {
        if !enabled {
            return Ok(Self::default());
        }
        let pipe = File::from(io::stdin().as_fd().try_clone_to_owned()?);
        fcntl_setfl(&pipe, fcntl_getfl(&pipe)? | OFlags::NONBLOCK)?;
        Ok(Self { pipe: Some(pipe) })
    }

    /// Disarm cancellation on confirmation, or request cleanup on EOF.
    pub fn cancelled(&mut self) -> io::Result<bool> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(false);
        };
        let mut reply = [0];
        match pipe.read(&mut reply) {
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(err) => Err(err),
            Ok(count) => {
                self.pipe = None;
                Ok(count == 0 || reply[0] != CONFIRMED)
            }
        }
    }
}
