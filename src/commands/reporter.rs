use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::reporter::control;
use crate::reporter::lock;
use crate::reporter::poller;
use crate::reporter::startup;
use crate::requirements;

/// Allow an in-progress reconciliation to finish before handling control requests.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
const START_POLL: Duration = Duration::from_millis(100);

/// Run the workspace reporter.
pub fn run(once: bool, verbose: bool, handshake: bool) -> Result<()> {
    // Set the reporter's creation mask before binding sockets or starting threads.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    if !handshake {
        requirements::warn();
    }
    poller::run(once, verbose, startup::Startup::from_stdin(handshake)?)
}

/// Spawn a detached reporter and wait until it answers a readiness request.
pub fn start() -> Result<()> {
    let path = lock::lock_path().with_extension("sock");
    if control::request(&path, control::PING, CONTROL_TIMEOUT)? {
        println!("reporter already running for this session");
        return Ok(());
    }
    requirements::warn();
    let log_path = lock::lock_path().with_extension("log");
    let mut child = spawn_reporter(&log_path)?;
    if let Err(err) = await_startup(&mut child, &path) {
        let cleanup = match await_exit(&mut child) {
            Ok(()) => String::new(),
            Err(err) => format!("; {err}"),
        };
        return Err(io::Error::other(format!("{err}{cleanup}; see {}", log_path.display())).into());
    }
    println!("reporter ready, logging to {}", log_path.display());
    Ok(())
}

/// Detach a reporter with a private startup pipe and output redirected to its log.
fn spawn_reporter(log_path: &Path) -> Result<Child> {
    // Created here, before the reporter it spawns can apply its own creation mask.
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["reporter", "--startup-handshake"])
        .stdin(Stdio::piped())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe and touches no Rust shared state.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(io::Error::from)
        });
    }
    Ok(command.spawn()?)
}

/// Confirm readiness, closing the private pipe on every return to cancel failed startup.
fn await_startup(child: &mut Child, path: &Path) -> Result<()> {
    let mut handshake = child.stdin.take().expect("piped startup stdin");
    await_ready(child, path)?;
    match handshake.write_all(&[startup::CONFIRMED]) {
        Ok(()) => Ok(()),
        // A competing reporter may be ready after our child loses the lock and exits.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Wait for a readiness reply while checking for failed launches.
fn await_ready(child: &mut Child, path: &Path) -> Result<()> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "reporter readiness timed out").into(),
            );
        }
        // A competing launch may own the socket even if our child has exited.
        if control::request(path, control::PING, remaining)? {
            return Ok(());
        }
        if let Some(status) = child.try_wait()?
            && lock::try_claim(&lock::lock_path())?.is_some()
        {
            return Err(io::Error::other(format!("reporter exited with {status}")).into());
        }
        thread::sleep(START_POLL);
    }
}

/// Reap the cancelled reporter without interrupting its resource cleanup.
fn await_exit(child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "startup cancelled but reporter cleanup is still pending",
            )
            .into());
        }
        thread::sleep(START_POLL);
    }
    Ok(())
}

/// Ask the reporter to clean up and acknowledge shutdown.
pub fn stop() -> Result<()> {
    let path = lock::lock_path().with_extension("sock");
    if control::request(&path, control::STOP, CONTROL_TIMEOUT)? {
        println!("reporter stopped");
    } else if lock::try_claim(&lock::lock_path())?.is_some() {
        println!("no reporter running for this session");
    } else {
        return Err(io::Error::other(
            "reporter lock is held but its control socket is unavailable; it may still be starting or use an older binary",
        ).into());
    }
    Ok(())
}
