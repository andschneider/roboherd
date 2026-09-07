use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// An isolated session with fake herdr and a long-lived roborev stream.
struct Session {
    dir: TempDir,
}

impl Session {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        script(
            &dir.path().join("herdr"),
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'herdr 0.8.2'; else exit 1; fi\n",
        );
        script(
            &dir.path().join("roborev"),
            "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'roborev v0.65.0'; exit; fi\necho $$ >> \"$STREAM_PIDS\"\nexec /bin/sleep 300\n",
        );
        Self { dir }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_roboherd"));
        cmd.arg(command)
            .env("HERDR_SOCKET_PATH", self.dir.path().join("herdr.sock"))
            .env("STREAM_PIDS", self.dir.path().join("streams"))
            .env("HERDR_BIN_PATH", self.dir.path().join("herdr"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.path().display()),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn run(&self, command: &str) -> Output {
        self.command(command).output().unwrap()
    }

    fn stream_pid(&self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(pids) = fs::read_to_string(self.dir.path().join("streams"))
                && let Some(pid) = pids.lines().next().and_then(|pid| pid.parse().ok())
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "stream did not start");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.run("stop-reporter");
    }
}

fn script(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn concurrent_starts_share_one_reporter_and_stop_reaps_its_stream() {
    let session = Session::new();
    let starts: Vec<_> = (0..4)
        .map(|_| session.command("start-reporter").spawn().unwrap())
        .collect();
    for start in starts {
        success(start.wait_with_output().unwrap());
    }
    let pid = session.stream_pid();
    success(session.run("start-reporter"));
    assert_eq!(
        fs::read_to_string(session.dir.path().join("streams"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    success(session.run("stop-reporter"));
    let target = rustix::process::Pid::from_raw(pid).unwrap();
    assert_eq!(
        rustix::process::test_kill_process(target),
        Err(rustix::io::Errno::SRCH)
    );
    assert!(!session.dir.path().join("roboherd-reporter.sock").exists());
    success(session.run("stop-reporter"));
    success(session.run("start-reporter"));
    success(session.run("stop-reporter"));
}

#[test]
fn stale_pid_and_socket_do_not_target_an_unrelated_process() {
    let session = Session::new();
    fs::write(
        session.dir.path().join("roboherd-reporter.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    drop(UnixListener::bind(session.dir.path().join("roboherd-reporter.sock")).unwrap());
    success(session.run("stop-reporter"));
    success(session.run("start-reporter"));
    success(session.run("stop-reporter"));
}

#[test]
fn held_lock_without_control_socket_reports_failure() {
    let session = Session::new();
    let lock = fs::File::create(session.dir.path().join("roboherd-reporter.lock")).unwrap();
    lock.try_lock().unwrap();
    let output = session.run("stop-reporter");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("control socket is unavailable"));
}

#[test]
fn stopping_one_session_leaves_the_other_stream_alive() {
    let first = Session::new();
    let second = Session::new();
    success(first.run("start-reporter"));
    success(second.run("start-reporter"));
    let second_pid = rustix::process::Pid::from_raw(second.stream_pid()).unwrap();
    success(first.run("stop-reporter"));
    assert!(rustix::process::test_kill_process(second_pid).is_ok());
    success(second.run("stop-reporter"));
}

#[test]
fn unavailable_socket_with_stale_pid_reports_failure() {
    let session = Session::new();
    fs::write(
        session.dir.path().join("roboherd-reporter.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    fs::create_dir(session.dir.path().join("roboherd-reporter.sock")).unwrap();
    let output = session.run("start-reporter");
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
}

#[test]
fn failed_child_start_reports_the_log_location() {
    let session = Session::new();
    fs::create_dir(session.dir.path().join("roboherd-reporter.lock")).unwrap();
    let output = session.run("start-reporter");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("roboherd-reporter.log"));
}

#[test]
fn failed_readiness_cleans_up_only_the_spawned_reporter() {
    let session = Session::new();
    let other = Session::new();
    success(other.run("start-reporter"));
    let other_pid = rustix::process::Pid::from_raw(other.stream_pid()).unwrap();
    script(
        &session.dir.path().join("herdr"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'herdr 0.8.2'; else exec /bin/sleep 2; fi\n",
    );
    let mut child = session
        .command("reporter")
        .arg("--startup-handshake")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let stream_pid = rustix::process::Pid::from_raw(session.stream_pid()).unwrap();
    // A failed readiness check closes this pipe without confirming startup.
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "cancelled reporter did not exit");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        rustix::process::test_kill_process(stream_pid),
        Err(rustix::io::Errno::SRCH)
    );
    assert!(!session.dir.path().join("roboherd-reporter.sock").exists());
    let lock = fs::File::options()
        .write(true)
        .open(session.dir.path().join("roboherd-reporter.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(rustix::process::test_kill_process(other_pid).is_ok());
    success(other.run("stop-reporter"));
}

#[test]
fn queued_clients_are_answered_together_after_slow_reconciliation() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let session = Session::new();
    let snapshot = serde_json::json!({"result": {"snapshot": {
        "workspaces": [{"workspace_id": "w", "label": "test", "active_tab_id": "t",
            "worktree": {"checkout_path": session.dir.path()}}], "panes": []
    }}});
    fs::write(
        session.dir.path().join("snapshot.json"),
        snapshot.to_string(),
    )
    .unwrap();
    script(
        &session.dir.path().join("herdr"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'herdr 0.8.2'; exit; fi\nif [ \"$2\" = snapshot ]; then\n echo pass >> \"$TEST_SESSION/passes\"\n /bin/sleep 3\n /bin/cat \"$TEST_SESSION/snapshot.json\"\nelse\n /bin/sleep 3\nfi\n",
    );
    script(
        &session.dir.path().join("git"),
        "#!/bin/sh\n/bin/sleep 3\nif [ \"$2\" = --show-current ]; then echo main; else echo \"$TEST_SESSION\"; fi\n",
    );
    script(
        &session.dir.path().join("roborev"),
        "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'roborev v0.65.0'; exit; fi\nif [ \"$1\" = stream ]; then\n echo $$ >> \"$STREAM_PIDS\"\n exec /bin/sleep 300\nelse\n /bin/sleep 3\n echo '[]'\nfi\n",
    );
    let mut reporter = session
        .command("reporter")
        .env("TEST_SESSION", session.dir.path())
        .spawn()
        .unwrap();
    session.stream_pid();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.dir.path().join("passes").exists() {
        assert!(
            Instant::now() < deadline,
            "slow reconciliation did not start"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let path = session.dir.path().join("roboherd-reporter.sock");
    let mut pings: Vec<_> = (0..4)
        .map(|_| {
            let mut connection = UnixStream::connect(&path).unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            connection.write_all(b"p").unwrap();
            connection
        })
        .collect();
    let mut stop = UnixStream::connect(&path).unwrap();
    stop.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stop.write_all(b"s").unwrap();
    for ping in &mut pings {
        let mut reply = [0];
        ping.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"r");
    }
    let mut reply = [0];
    stop.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"d");
    assert!(reporter.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(session.dir.path().join("passes"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn start_reporter_creates_a_private_log_and_lock() {
    use std::os::unix::process::CommandExt;

    let session = Session::new();
    let mut command = session.command("start-reporter");
    // SAFETY: umask is async-signal-safe and touches no Rust shared state.
    unsafe {
        command.pre_exec(|| {
            rustix::process::umask(rustix::fs::Mode::empty());
            Ok(())
        });
    }
    success(command.output().unwrap());
    for name in ["roboherd-reporter.log", "roboherd-reporter.lock"] {
        let mode = fs::metadata(session.dir.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{name} permits group or other access");
    }
    success(session.run("stop-reporter"));
}

#[test]
fn reporter_uses_private_modes_with_a_permissive_parent_umask() {
    use std::os::unix::process::CommandExt;

    let session = Session::new();
    let mut command = session.command("reporter");
    // SAFETY: umask is async-signal-safe and touches no Rust shared state.
    unsafe {
        command.pre_exec(|| {
            rustix::process::umask(rustix::fs::Mode::empty());
            Ok(())
        });
    }
    let mut reporter = command.spawn().unwrap();
    session.stream_pid();
    for name in [
        "roboherd-reporter.sock",
        "roboherd-reporter.lock",
        "roboherd-reporter.status",
        "streams",
    ] {
        let mode = fs::metadata(session.dir.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{name} permits group or other access");
    }
    success(session.run("stop-reporter"));
    assert!(reporter.wait().unwrap().success());
}

#[test]
fn reporter_status_file_describes_the_live_process() {
    let session = Session::new();
    success(session.run("start-reporter"));
    session.stream_pid();
    let path = session.dir.path().join("roboherd-reporter.status");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        let status: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        if !status["last_pass_at"].is_null() {
            break status;
        }
        assert!(Instant::now() < deadline, "status pass was not published");
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert!(status["pid"].as_u64().is_some_and(|pid| pid > 0));
    assert!(status["errors"].as_array().is_some_and(|errors| {
        errors
            .iter()
            .any(|error| error.as_str().is_some_and(|error| error.contains("herdr")))
    }));
    assert!(status["stream_error"].is_null());
    success(session.run("stop-reporter"));
    assert!(!path.exists());
}

#[test]
fn doctor_reports_the_current_session_reporter() {
    let session = Session::new();
    script(
        &session.dir.path().join("herdr"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'herdr 0.8.2'; else echo '{\"result\":{\"snapshot\":{\"workspaces\":[{\"workspace_id\":\"w1\",\"label\":\"api\",\"active_tab_id\":\"w1:t1\",\"tokens\":{\"roborev_p\":\"chk1\"}}],\"panes\":[]}}}'; fi\n",
    );
    success(session.run("start-reporter"));
    let output = session.run("doctor");
    let report = String::from_utf8_lossy(&output.stdout).into_owned();
    success(output);
    assert!(report.contains("Tools"));
    assert!(report.contains("herdr 0.8.2"));
    assert!(report.contains("Reporter"));
    assert!(report.contains("roborev stream running"));
    assert!(report.contains("Workspaces"));
    assert!(report.contains("api (w1): chk1"));
    success(session.run("stop-reporter"));
}

#[test]
fn doctor_fails_when_the_session_reporter_is_absent() {
    let session = Session::new();
    let output = session.run("doctor");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reporter is not running"));
}

#[test]
fn reporter_retries_when_roborev_is_missing_at_startup() {
    let session = Session::new();
    fs::remove_file(session.dir.path().join("roborev")).unwrap();
    let start = session.run("start-reporter");
    assert!(String::from_utf8_lossy(&start.stderr).contains("warning"));
    success(start);
    let output = session.run("doctor");
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("warn roborev version could not be verified"));
    assert!(report.contains("Reporter"));
    assert!(report.contains("failed to spawn roborev"));
    success(session.run("stop-reporter"));
}

#[test]
fn doctor_reports_the_reporter_after_a_tool_failure() {
    let session = Session::new();
    success(session.run("start-reporter"));
    script(
        &session.dir.path().join("roborev"),
        "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'roborev v0.62.9'; fi\n",
    );
    let output = session.run("doctor");
    assert!(!output.status.success());
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("fail roborev 0.62.9 requires 0.63.0 or newer"));
    assert!(report.contains("Reporter"));
    assert!(report.contains("roborev stream running"));
    success(session.run("stop-reporter"));
}
