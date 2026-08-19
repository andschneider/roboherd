//! Every subprocess the plugin runs starts here, with explicit argv and never a shell.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Captured stdout and stderr of a finished command.
#[derive(Debug, Clone)]
pub struct Output {
    pub stdout: String,
    /// Populated on every run, not only a failed one: some CLIs put status text here on success.
    pub stderr: String,
}

/// How often a timed command's exit status is checked.
const POLL_TICK: Duration = Duration::from_millis(25);

/// Run a command with explicit argv and capture its output. Nothing is passed through a shell, so
/// selected text and paths never reach a command interpreter.
pub fn run<S>(program: &str, args: &[S], cwd: Option<&Path>) -> Result<Output>
where
    S: AsRef<OsStr>,
{
    execute(program, args, cwd, None)
}

/// Run a command whose output is displayed rather than parsed, replacing invalid UTF-8 instead of
/// failing on it.
pub fn run_lossy<S>(program: &str, args: &[S], cwd: Option<&Path>) -> Result<Output>
where
    S: AsRef<OsStr>,
{
    let (stdout, stderr) = capture(program, args, cwd, None, None)?;
    Ok(Output {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Run a command, killing it if it outlives `timeout`. Commands on the reporter's poll path are
/// timed so that one unresponsive checkout cannot stall a whole pass.
pub fn run_timed<S>(
    program: &str,
    args: &[S],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<Output>
where
    S: AsRef<OsStr>,
{
    execute(program, args, cwd, Some(timeout))
}

/// Run a command under a timeout and deserialize its stdout as JSON.
pub fn run_json_timed<S, T>(
    program: &str,
    args: &[S],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<T>
where
    S: AsRef<OsStr>,
    T: DeserializeOwned,
{
    decode_json(program, execute(program, args, cwd, Some(timeout))?)
}

/// Run a command for its side effect and discard the captured output.
pub fn run_ok<S>(program: &str, args: &[S], cwd: Option<&Path>) -> Result<()>
where
    S: AsRef<OsStr>,
{
    execute(program, args, cwd, None).map(|_| ())
}

/// Run a command under a timeout for its side effect and discard the captured output.
pub fn run_ok_timed<S>(
    program: &str,
    args: &[S],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<()>
where
    S: AsRef<OsStr>,
{
    execute(program, args, cwd, Some(timeout)).map(|_| ())
}

/// Run a command under a timeout with `input` piped to its stdin, discarding captured output.
///
/// For a clipboard tool that reads until EOF, closing stdin (dropping the writer) is what tells it
/// the input is complete.
pub fn run_stdin_ok_timed<S>(
    program: &str,
    args: &[S],
    input: &[u8],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<()>
where
    S: AsRef<OsStr>,
{
    capture(program, args, cwd, Some(timeout), Some(input)).map(|_| ())
}

/// Spawn a long-lived command whose stdout is read line by line rather than collected.
///
/// stderr is inherited, not piped, because nothing here drains it and a full pipe would wedge the
/// child.
pub fn spawn_streaming<S>(program: &str, args: &[S]) -> Result<Child>
where
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|source| Error::Spawn {
            program: program.to_string(),
            source,
        })
}

fn execute<S>(
    program: &str,
    args: &[S],
    cwd: Option<&Path>,
    timeout: Option<Duration>,
) -> Result<Output>
where
    S: AsRef<OsStr>,
{
    let (stdout, stderr) = capture(program, args, cwd, timeout, None)?;
    Ok(Output {
        stdout: decode(program, stdout)?,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Run a command to completion, leaving stdout undecoded for the caller to interpret. stderr is
/// always returned alongside it, since a successful run can still put human-readable status there.
///
/// `input`, when given, is piped to stdin and the pipe is closed once it's written, which is what
/// tells a command reading until EOF (e.g. a clipboard tool) that the input is complete.
fn capture<S>(
    program: &str,
    args: &[S],
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    input: Option<&[u8]>,
) -> Result<(Vec<u8>, Vec<u8>)>
where
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(match input {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let mut child = command.spawn().map_err(|source| Error::Spawn {
        program: program.to_string(),
        source,
    })?;

    // A child that fills a pipe buffer blocks until drained, outlasting any timeout enforced here.
    // Feeding stdin on its own thread guards the same way against a child that only starts reading
    // once it has produced enough output to fill its own pipe.
    let stdin_writer = input.map(|bytes| feed(child.stdin.take(), bytes.to_vec()));
    let stdout_reader = drain(child.stdout.take());
    let stderr_reader = drain(child.stderr.take());

    let status = match timeout {
        None => Some(child.wait().map_err(|source| Error::CommandOutput {
            program: program.to_string(),
            source,
        })?),
        Some(limit) => {
            let status = wait_until(&mut child, limit, program)?;
            if status.is_none() {
                // Killing the child closes its pipes, releasing the reader threads. Roborev's
                // detached daemon is the one grandchild here and starts with null stdio, so no
                // surviving process holds them open.
                let _ = child.kill();
                let _ = child.wait();
            }
            status
        }
    };

    let stdout = join(stdout_reader, program)?;
    let stderr = join(stderr_reader, program)?;

    let Some(status) = status else {
        return Err(Error::CommandTimeout {
            program: program.to_string(),
            seconds: timeout.unwrap_or_default().as_secs(),
        });
    };

    if !status.success() {
        return Err(Error::CommandFailed {
            program: program.to_string(),
            code: match status.code() {
                Some(code) => code.to_string(),
                None => "a signal".to_string(),
            },
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        });
    }

    // A command that exits 0 without draining its stdin (unlikely for a clipboard tool, but not
    // ruled out) still counts as success; the input write failure would be moot at that point.
    if let Some(writer) = stdin_writer {
        join_input(writer, program)?;
    }

    Ok((stdout, stderr))
}

/// Wait for `child`, returning `None` if it is still running once `limit` has elapsed.
fn wait_until(child: &mut Child, limit: Duration, program: &str) -> Result<Option<ExitStatus>> {
    let deadline = Instant::now() + limit;
    loop {
        let finished = child.try_wait().map_err(|source| Error::CommandOutput {
            program: program.to_string(),
            source,
        })?;
        if let Some(status) = finished {
            return Ok(Some(status));
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        std::thread::sleep(POLL_TICK.min(deadline - now));
    }
}

/// Read a child pipe to end on its own thread.
fn drain<R>(pipe: Option<R>) -> JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut buffer)?;
        }
        Ok(buffer)
    })
}

fn join(reader: JoinHandle<std::io::Result<Vec<u8>>>, program: &str) -> Result<Vec<u8>> {
    reader
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("output reader thread panicked")))
        .map_err(|source| Error::CommandOutput {
            program: program.to_string(),
            source,
        })
}

/// Write `input` to a child pipe on its own thread, then drop it to close the pipe.
fn feed<W>(pipe: Option<W>, input: Vec<u8>) -> JoinHandle<std::io::Result<()>>
where
    W: Write + Send + 'static,
{
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            pipe.write_all(&input)?;
        }
        Ok(())
    })
}

fn join_input(writer: JoinHandle<std::io::Result<()>>, program: &str) -> Result<()> {
    writer
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("input writer thread panicked")))
        .map_err(|source| Error::CommandInput {
            program: program.to_string(),
            source,
        })
}

fn decode_json<T>(program: &str, output: Output) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(&output.stdout).map_err(|source| Error::CommandJson {
        program: program.to_string(),
        source,
    })
}

/// Run a command with the terminal attached, for full-screen programs that own the pane. Output is
/// not captured, so a non-zero exit carries no stderr to report.
pub fn run_interactive<S>(program: &str, args: &[S], cwd: Option<&Path>) -> Result<()>
where
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let status = command.status().map_err(|source| Error::Spawn {
        program: program.to_string(),
        source,
    })?;

    if !status.success() {
        return Err(Error::CommandFailed {
            program: program.to_string(),
            code: match status.code() {
                Some(code) => code.to_string(),
                None => "a signal".to_string(),
            },
            stderr: String::new(),
        });
    }

    Ok(())
}

fn decode(program: &str, bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| Error::CommandUtf8 {
        program: program.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::{run, run_json_timed, run_lossy, run_stdin_ok_timed, run_timed};
    use crate::error::Error;

    #[test]
    fn captures_stdout() {
        let output = run("echo", &["hello"], None).expect("echo runs");
        assert_eq!(output.stdout.trim(), "hello");
    }

    #[test]
    fn argv_is_not_shell_interpreted() {
        let output = run("echo", &["a; rm -rf /"], None).expect("echo runs");
        assert_eq!(output.stdout.trim(), "a; rm -rf /");
    }

    #[test]
    fn non_zero_exit_is_an_error() {
        let err = run("false", &[] as &[&str], None).expect_err("false exits non-zero");
        assert!(matches!(err, Error::CommandFailed { .. }));
    }

    #[test]
    fn missing_program_reports_spawn_failure() {
        let err = run("roboherd-no-such-program", &[] as &[&str], None)
            .expect_err("program does not exist");
        assert!(matches!(err, Error::Spawn { .. }));
    }

    /// Output that is parsed keeps its strict decode, so a replacement character never reaches a
    /// caller as data.
    #[test]
    fn stdout_that_is_not_utf8_is_an_error() {
        let err = run("printf", &["a\\377b"], None).expect_err("the output is not UTF-8");
        assert!(matches!(err, Error::CommandUtf8 { .. }), "{err:?}");
    }

    #[test]
    fn run_lossy_replaces_invalid_utf8() {
        let output = run_lossy("printf", &["a\\377b"], None).expect("printf runs");
        assert_eq!(output.stdout, "a\u{fffd}b");
    }

    /// stderr only ever describes a failure, so invalid UTF-8 in it must not replace that failure
    /// with a decoding error.
    ///
    /// git echoes a bad revision back verbatim. Tools that format a filename into the message
    /// instead are no use here: GNU coreutils shell-quotes the invalid bytes away while BSD passes
    /// them through, so the fixture would test the platform rather than the decode.
    #[test]
    #[cfg(unix)]
    fn a_failure_whose_stderr_is_not_utf8_still_reports_the_failure() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = TempDir::new().expect("tempdir");
        run("git", &["init", "--quiet"], Some(dir.path())).expect("git init runs");

        let revision = OsStr::from_bytes(b"bad\xffref");
        let err = run(
            "git",
            &[OsStr::new("cat-file"), OsStr::new("commit"), revision],
            Some(dir.path()),
        )
        .expect_err("the revision does not resolve");

        let Error::CommandFailed { stderr, .. } = &err else {
            panic!("{err:?}");
        };
        assert!(stderr.contains('\u{fffd}'), "{stderr:?}");
    }

    #[test]
    fn parses_json_stdout() {
        let value: Vec<u32> = run_json_timed("echo", &["[1,2,3]"], None, Duration::from_secs(30))
            .expect("valid JSON");
        assert_eq!(value, vec![1, 2, 3]);
    }

    #[test]
    fn a_command_that_overruns_its_timeout_is_killed() {
        let start = Instant::now();
        let err = run_timed("sleep", &["30"], None, Duration::from_millis(200))
            .expect_err("sleep outlives the timeout");

        assert!(matches!(err, Error::CommandTimeout { .. }));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout did not cut the command short"
        );
    }

    #[test]
    fn a_command_inside_its_timeout_still_returns_output() {
        let output = run_timed("echo", &["hi"], None, Duration::from_secs(30)).expect("echo runs");
        assert_eq!(output.stdout.trim(), "hi");
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        // A child writing more than the pipe buffer (64 KiB on Linux) blocks until drained. If the
        // reader threads were not draining concurrently, this would hang rather than fail. Stays
        // under Linux's 128 KiB MAX_ARG_STRLEN cap on a single argv element, which macOS has no
        // equivalent of.
        let payload = "x".repeat(100 * 1024);
        let output = run_timed("echo", &[payload.as_str()], None, Duration::from_secs(30))
            .expect("echo runs");
        assert_eq!(output.stdout.trim().len(), payload.len());
    }

    #[test]
    fn stdin_input_larger_than_a_pipe_buffer_does_not_deadlock() {
        // Mirrors the stdout-direction test above: `cat` echoes the input back, so the reader
        // thread must drain it concurrently with the writer thread feeding stdin, or both block.
        let payload = "y".repeat(100 * 1024).into_bytes();
        run_stdin_ok_timed(
            "cat",
            &[] as &[&str],
            &payload,
            None,
            Duration::from_secs(30),
        )
        .expect("cat runs");
    }

    #[test]
    fn a_failing_command_still_reports_failure_with_stdin_input() {
        let err = run_stdin_ok_timed("false", &[] as &[&str], b"hi", None, Duration::from_secs(5))
            .expect_err("false exits non-zero");
        assert!(matches!(err, Error::CommandFailed { .. }));
    }
}
