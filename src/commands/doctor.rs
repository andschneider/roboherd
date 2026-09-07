use std::collections::HashMap;
use std::fmt::Write;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::Result;
use crate::herdr;
use crate::reporter::lock;
use crate::reporter::poller::TOKEN_TTL;
use crate::reporter::schedule::POLL_INTERVAL;
use crate::reporter::status;
use crate::reporter::status::ReporterStatus;
use crate::requirements;
use crate::requirements::{Check, Level};
use crate::roborev;

/// How long the workspace snapshot read gets, for output that is diagnostic only.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Check the reporter serving the current herdr session.
pub fn run() -> Result<()> {
    let (tools, mut failed) = render_tools(&requirements::inspect());
    print!("{tools}");

    let (environment, environment_failed) = render_environment();
    print!("{environment}");
    failed |= environment_failed;

    let lock_path = lock::lock_path();
    let log_path = lock_path.with_extension("log");
    if lock::try_claim(&lock_path)?.is_some() {
        return Err(io::Error::other(format!(
            "reporter is not running for this session; see {}",
            log_path.display()
        ))
        .into());
    }
    let status_path = lock_path.with_extension("status");
    let runtime = match status::read(&status_path) {
        Ok(Some(status)) => status,
        Ok(None) => {
            return Err(io::Error::other(format!(
                "reporter status is unavailable; see {}",
                log_path.display()
            ))
            .into());
        }
        Err(err) => {
            return Err(io::Error::other(format!(
                "reporter status is unreadable: {err}; see {}",
                log_path.display()
            ))
            .into());
        }
    };
    let (report, reporter_failed) = render(&runtime, unix_seconds(), &log_path, binary_modified());
    print!("{report}");
    failed |= reporter_failed;

    print!("{}", render_workspaces());
    if failed {
        return Err(io::Error::other("doctor found problems").into());
    }
    Ok(())
}

/// Render external tool versions and whether any check failed.
fn render_tools(checks: &[Check]) -> (String, bool) {
    let mut report = String::from("Tools\n");
    let mut failed = false;
    for check in checks {
        line(&mut report, check.level.label(), &check.message);
        failed |= check.level == Level::Fail;
    }
    (report, failed)
}

/// Render the plugin config and the agents roborev can hand a review to.
///
/// Both fail silently at the point of use: a bad config key breaks inside a popup that closes
/// before it can be read, and no installed agent surfaces only when the commit picker is opened.
fn render_environment() -> (String, bool) {
    let mut report = String::from("Environment\n");
    let mut failed = false;

    match Config::path() {
        Some(path) => line(&mut report, "ok", &format!("config: {}", path.display())),
        None => line(&mut report, "warn", "config path could not be determined"),
    }
    match Config::load() {
        Ok(config) => line(
            &mut report,
            "ok",
            &format!("tui_placement: {:?}", config.tui_placement).to_lowercase(),
        ),
        Err(err) => {
            line(&mut report, "fail", &err.to_string());
            failed = true;
        }
    }

    let agents = roborev::installed_agents();
    if agents.is_empty() {
        line(
            &mut report,
            "warn",
            "no roborev-compatible agent found on PATH; the commit picker has nothing to offer",
        );
    } else {
        line(&mut report, "ok", &format!("agents: {}", agents.join(", ")));
    }

    (report, failed)
}

/// Render every open workspace's `$roborev_*` tokens, pretty-printed the way herdr's sidebar joins
/// them.
///
/// This is the only check that reads accepted state instead of the reporter's belief that it
/// published: a report can be rejected for a stale `seq`, a workspace can exhaust its metadata
/// source slots, or a pass can finish just as the TTL from the one before it expires, and
/// `last_pass_at` alone would miss all three. Emptiness is not judged: a workspace can hold nothing
/// because it has no roborev repo, or because a tracked repo has nothing currently open, and both
/// are normal, so an empty line reads as a plain fact rather than a verdict.
fn render_workspaces() -> String {
    let mut report = String::from("Workspaces\n");
    match herdr::snapshot(SNAPSHOT_TIMEOUT) {
        Ok(snapshot) => {
            for workspace in &snapshot.workspaces {
                line(
                    &mut report,
                    "ok",
                    &format!(
                        "{} ({}): {}",
                        workspace.label,
                        workspace.workspace_id,
                        describe_tokens(workspace.tokens.as_ref())
                    ),
                );
            }
        }
        Err(err) => line(
            &mut report,
            "warn",
            &format!("workspace tokens unavailable: {err}"),
        ),
    }
    report
}

/// Render one workspace's tokens the way herdr's sidebar joins them, or a bare dash when it holds
/// none.
fn describe_tokens(tokens: Option<&HashMap<String, String>>) -> String {
    let rendered: Vec<&str> = herdr::TOKENS
        .iter()
        .filter_map(|name| tokens?.get(*name))
        .map(String::as_str)
        .collect();
    if rendered.is_empty() {
        "\u{2014}".to_string()
    } else {
        rendered.join(" · ")
    }
}

/// Render reporter facts and whether any check failed.
fn render(
    status: &ReporterStatus,
    now: u64,
    log_path: &Path,
    binary_modified: Option<u64>,
) -> (String, bool) {
    let mut report = String::from("Reporter\n");
    let mut failed = false;

    let version = env!("CARGO_PKG_VERSION");
    let level = if status.version == version {
        "ok"
    } else {
        "warn"
    };
    line(
        &mut report,
        level,
        &format!(
            "roboherd {} (CLI {version}), pid {}, up {}",
            status.version,
            status.pid,
            elapsed(now.saturating_sub(status.started_at))
        ),
    );

    line(&mut report, "ok", &format!("log: {}", log_path.display()));

    // The version string only moves at a release, so a rebuilt binary between releases looks
    // identical to the running reporter here. Its mtime does not: newer than `started_at` means
    // the lock holder is still running the code from before the rebuild.
    if let Some(modified) = binary_modified
        && modified > status.started_at
    {
        line(
            &mut report,
            "warn",
            &format!(
                "bin/roboherd was rebuilt {} after this reporter started; it is running stale \
                 code. Run stop-reporter and then start-reporter",
                elapsed(modified.saturating_sub(status.started_at))
            ),
        );
    }

    match (status.last_pass_at, status.errors.is_empty()) {
        (None, _) => line(&mut report, "warn", "no reconciliation pass completed yet"),
        // Past the token TTL herdr has already expired the badge, so a pass this old is the
        // symptom rather than a slow tick.
        (Some(finished_at), true) if now.saturating_sub(finished_at) >= TOKEN_TTL.as_secs() => {
            line(
                &mut report,
                "fail",
                &format!(
                    "reporter is not reconciling, last pass {} ago (due every {}). Run \
                     stop-reporter and then start-reporter",
                    elapsed(now.saturating_sub(finished_at)),
                    elapsed(POLL_INTERVAL.as_secs())
                ),
            );
            failed = true;
        }
        (Some(finished_at), true) => line(
            &mut report,
            "ok",
            &format!(
                "last reconciliation completed {} ago",
                elapsed(now.saturating_sub(finished_at))
            ),
        ),
        (Some(_), false) => {
            for error in &status.errors {
                line(&mut report, "fail", error);
            }
            failed = true;
        }
    }

    // Poll is the source of truth; a dropped stream costs latency and nothing else.
    match status.stream_error.as_deref() {
        None => line(&mut report, "ok", "roborev stream running"),
        Some(error) => line(&mut report, "warn", error),
    }

    (report, failed)
}

fn line(report: &mut String, level: &str, message: &str) {
    writeln!(report, "  {level:<4} {message}").expect("writing to a string cannot fail");
}

fn elapsed(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// When the running binary was last rebuilt, absent when either stat cannot be read.
fn binary_modified() -> Option<u64> {
    let exe = std::env::current_exe().ok()?;
    let modified = std::fs::metadata(exe).ok()?.modified().ok()?;
    modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::{TOKEN_TTL, describe_tokens, render, render_tools};
    use crate::reporter::status::ReporterStatus;
    use crate::requirements::{Check, Level};

    fn healthy() -> ReporterStatus {
        ReporterStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: 42,
            started_at: 40,
            last_pass_at: Some(100),
            errors: Vec::new(),
            stream_error: None,
        }
    }

    fn render_healthy(status: &ReporterStatus, now: u64) -> (String, bool) {
        render(status, now, Path::new("/tmp/roboherd.log"), None)
    }

    #[test]
    fn healthy_reporter_passes() {
        let (report, failed) = render_healthy(&healthy(), 105);
        assert!(!failed);
        assert!(report.contains("last reconciliation completed 5s ago"));
        assert!(report.contains("roborev stream running"));
        assert!(report.contains("log: /tmp/roboherd.log"));
    }

    #[test]
    fn a_binary_rebuilt_after_the_reporter_started_warns_but_does_not_fail() {
        let status = healthy();
        let (report, failed) = render(&status, 105, Path::new("/tmp/roboherd.log"), Some(90));
        assert!(!failed);
        assert!(report.contains("warn bin/roboherd was rebuilt 50s after this reporter started"));
    }

    #[test]
    fn a_binary_older_than_the_reporter_is_not_reported() {
        let status = healthy();
        let (report, _) = render(&status, 105, Path::new("/tmp/roboherd.log"), Some(10));
        assert!(!report.contains("was rebuilt"));
    }

    #[test]
    fn tokens_are_joined_in_sidebar_order() {
        let mut tokens = HashMap::new();
        tokens.insert("roborev_p".to_string(), "\u{2713}1".to_string());
        tokens.insert("roborev_f".to_string(), "\u{d7}1".to_string());
        assert_eq!(describe_tokens(Some(&tokens)), "\u{d7}1 · \u{2713}1");
    }

    #[test]
    fn absent_and_empty_tokens_both_read_as_a_dash() {
        assert_eq!(describe_tokens(None), "\u{2014}");
        assert_eq!(describe_tokens(Some(&HashMap::new())), "\u{2014}");
    }

    #[test]
    fn tool_failures_and_unknown_versions_are_reported_together() {
        let checks = [
            Check {
                level: Level::Fail,
                message: "herdr 0.7.4 requires 0.7.5 or newer".to_string(),
            },
            Check {
                level: Level::Warn,
                message: "roborev version could not be verified: abcdef1-dirty".to_string(),
            },
        ];
        let (report, failed) = render_tools(&checks);
        assert!(failed);
        assert!(report.contains("fail herdr 0.7.4 requires 0.7.5 or newer"));
        assert!(report.contains("warn roborev version could not be verified"));
    }

    #[test]
    fn a_pass_older_than_the_badge_ttl_fails() {
        let status = healthy();
        let (report, failed) = render_healthy(&status, 100 + TOKEN_TTL.as_secs());
        assert!(failed);
        assert!(report.contains("fail reporter is not reconciling, last pass 45s ago"));
        assert!(report.contains("Run stop-reporter and then start-reporter"));
    }

    #[test]
    fn failed_pass_fails_doctor_but_stream_error_only_warns() {
        let mut status = healthy();
        status.errors = vec!["workspace w1 (api) failed: herdr failed".to_string()];
        status.stream_error = Some("stream failed".to_string());
        let (report, failed) = render_healthy(&status, 105);
        assert!(failed);
        assert!(report.contains("fail workspace w1 (api) failed: herdr failed"));
        assert!(report.contains("warn stream failed"));
    }

    #[test]
    fn every_workspace_failure_gets_its_own_line() {
        let mut status = healthy();
        status.errors = vec![
            "workspace w1 (api) failed: timed out".to_string(),
            "workspace w2 (web) failed: no checkout".to_string(),
        ];
        let (report, failed) = render_healthy(&status, 105);
        assert!(failed);
        assert!(report.contains("fail workspace w1 (api) failed: timed out"));
        assert!(report.contains("fail workspace w2 (web) failed: no checkout"));
    }
}
