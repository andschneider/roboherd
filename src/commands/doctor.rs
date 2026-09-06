use std::fmt::Write;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::reporter::lock;
use crate::reporter::status;
use crate::reporter::status::ReporterStatus;
use crate::requirements;
use crate::requirements::{Check, Level};

/// Check the reporter serving the current herdr session.
pub fn run() -> Result<()> {
    let (tools, mut failed) = render_tools(&requirements::inspect());
    print!("{tools}");

    let lock_path = lock::lock_path();
    if lock::try_claim(&lock_path)?.is_some() {
        return Err(io::Error::other("reporter is not running for this session").into());
    }
    let log_path = lock_path.with_extension("log");
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
    let (report, reporter_failed) = render(&runtime, unix_seconds());
    print!("{report}");
    failed |= reporter_failed;
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

/// Render reporter facts and whether any check failed.
fn render(status: &ReporterStatus, now: u64) -> (String, bool) {
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

    match (status.last_pass_at, status.last_error.as_deref()) {
        (None, _) => line(&mut report, "warn", "no reconciliation pass completed yet"),
        (Some(finished_at), None) => line(
            &mut report,
            "ok",
            &format!(
                "last reconciliation completed {} ago",
                elapsed(now.saturating_sub(finished_at))
            ),
        ),
        (_, Some(error)) => {
            line(&mut report, "fail", error);
            failed = true;
        }
    }

    match status.stream_error.as_deref() {
        None => line(&mut report, "ok", "roborev stream running"),
        Some(error) => {
            line(&mut report, "fail", error);
            failed = true;
        }
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

#[cfg(test)]
mod tests {
    use super::{render, render_tools};
    use crate::reporter::status::ReporterStatus;
    use crate::requirements::{Check, Level};

    fn healthy() -> ReporterStatus {
        ReporterStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: 42,
            started_at: 40,
            last_pass_at: Some(100),
            last_error: None,
            stream_error: None,
        }
    }

    #[test]
    fn healthy_reporter_passes() {
        let (report, failed) = render(&healthy(), 105);
        assert!(!failed);
        assert!(report.contains("last reconciliation completed 5s ago"));
        assert!(report.contains("roborev stream running"));
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
    fn failed_pass_and_stream_fail_doctor() {
        let mut status = healthy();
        status.last_error = Some("herdr failed".to_string());
        status.stream_error = Some("stream failed".to_string());
        let (report, failed) = render(&status, 105);
        assert!(failed);
        assert!(report.contains("fail herdr failed"));
        assert!(report.contains("fail stream failed"));
    }
}
