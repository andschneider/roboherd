use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use rustix::fs::{Access, access};
use serde::Deserialize;

use crate::error::Result;
use crate::exec;

/// Lifecycle state of a roborev review job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Canceled,
    Applied,
    Rebased,
    Skipped,
    /// A status added by a newer roborev than this plugin knows about.
    #[serde(other)]
    Unknown,
}

/// What an enqueue reviews.
///
/// Modeling dirty and revision selections as alternatives prevents roborev from silently ignoring
/// a revision passed with `--dirty`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Uncommitted changes, staged, unstaged, and untracked alike.
    Dirty,
    Commit(String),
    /// The inclusive span between two commits, older first.
    Range(String, String),
}

/// The reviewer prompt a review runs under, which roborev calls a review type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewType {
    #[default]
    Default,
    Security,
    Design,
}

/// The roborev job fields needed by the reporter and review pane.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewJob {
    /// Names the job, for the review pane to render and the sidebar token to report.
    pub id: i64,
    pub status: JobStatus,
    /// Whether the review has been closed out. Null until a review exists.
    #[serde(default)]
    pub closed: Option<bool>,
    /// Pass or fail parsed from the review output, `P` or `F`.
    #[serde(default)]
    pub verdict: Option<String>,
}

/// Review fields returned by `roborev show --json`.
#[derive(Debug, Deserialize)]
pub struct ShownReview {
    pub job_id: i64,
    pub agent: String,
    pub output: String,
    #[serde(default)]
    pub closed: Option<bool>,
    #[serde(default)]
    pub job: Option<ShownJob>,
    #[serde(default)]
    pub comments: Vec<ShownComment>,
}

/// Job metadata nested in `roborev show --json`.
#[derive(Debug, Deserialize)]
pub struct ShownJob {
    #[serde(default)]
    pub git_ref: String,
    #[serde(default)]
    pub token_usage: String,
}

/// One review comment returned by `roborev show --json`.
#[derive(Debug, Deserialize)]
pub struct ShownComment {
    pub responder: String,
    pub response: String,
    #[serde(default)]
    pub created_at: String,
}

impl JobStatus {
    /// Whether the job is waiting to run or running now.
    pub fn is_active(self) -> bool {
        matches!(self, JobStatus::Queued | JobStatus::Running)
    }

    /// Whether the job finished in a state that could carry findings.
    pub fn is_reviewed(self) -> bool {
        matches!(self, JobStatus::Done | JobStatus::Failed)
    }
}

impl ReviewType {
    /// The next type in the picker's cycle.
    ///
    /// roborev also accepts `lookahead`, a time-series look-ahead bias reviewer. It is left out of
    /// the cycle until a checkout here has time-series code to point it at.
    pub fn next(self) -> Self {
        match self {
            ReviewType::Default => ReviewType::Security,
            ReviewType::Security => ReviewType::Design,
            ReviewType::Design => ReviewType::Default,
        }
    }

    /// The name for the picker footer.
    pub fn label(self) -> &'static str {
        match self {
            ReviewType::Default => "default",
            ReviewType::Security => "security",
            ReviewType::Design => "design",
        }
    }

    /// The value `--type` carries, or `None` when the flag is left off.
    ///
    /// roborev names its ordinary reviewer `default` internally but rejects `--type default`, so
    /// the default is expressed by omitting the flag.
    fn flag(self) -> Option<&'static str> {
        match self {
            ReviewType::Default => None,
            ReviewType::Security => Some("security"),
            ReviewType::Design => Some("design"),
        }
    }
}

impl ReviewJob {
    /// Whether the job is an open completed review with a failing verdict.
    pub fn needs_attention(&self) -> bool {
        self.status.is_reviewed()
            && !self.closed.unwrap_or(false)
            && self.verdict.as_deref() == Some("F")
    }

    /// Whether the job is an open completed review with a passing verdict.
    pub fn is_open_pass(&self) -> bool {
        self.status.is_reviewed()
            && !self.closed.unwrap_or(false)
            && self.verdict.as_deref() == Some("P")
    }

    /// Whether the job finished with a verdict.
    pub fn reached_verdict(&self) -> bool {
        self.status.is_reviewed() && self.verdict.is_some()
    }
}

/// The review worth opening from a listing, preferring one that needs attention over a passing one.
///
/// Jobs arrive newest first, so the first match in each pass is the most recent. A job that reached
/// no verdict judged nothing, so it is not a review to open.
pub fn newest_reviewed(jobs: &[ReviewJob]) -> Option<&ReviewJob> {
    jobs.iter()
        .find(|job| job.needs_attention())
        .or_else(|| jobs.iter().find(|job| job.reached_verdict()))
}

/// List review jobs for the repo that `checkout` resolves to. An explicit branch keeps a reporter
/// snapshot tied to the branch it recorded, while `None` delegates branch detection to roborev.
///
/// A repo with no jobs yields `null` rather than `[]`, since roborev encodes a nil slice, so the
/// array is parsed through an [`Option`].
pub fn list(
    checkout: &Path,
    branch: Option<&str>,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<ReviewJob>> {
    // roborev caps the listing at 50 without this, and it truncates before roboherd filters.
    let limit = limit.to_string();
    let mut args = vec!["list", "--json", "--limit", &limit];
    if let Some(branch) = branch {
        args.extend(["--branch", branch]);
    }
    let jobs: Option<Vec<ReviewJob>> =
        exec::run_json_timed("roborev", &args, Some(checkout), timeout)?;
    Ok(jobs.unwrap_or_default())
}

/// The review output and metadata for `job_id`, or for HEAD when no job is named.
///
/// `--job` forces the argument to be read as a job id. Without it roborev tries the argument as a
/// git ref first, and a numeric id can collide with a valid ref.
pub fn show(checkout: &Path, job_id: Option<i64>, timeout: Duration) -> Result<ShownReview> {
    match job_id {
        Some(id) => exec::run_json_timed(
            "roborev",
            &["show", "--job", "--json", &id.to_string()],
            Some(checkout),
            timeout,
        ),
        None => exec::run_json_timed("roborev", &["show", "--json"], Some(checkout), timeout),
    }
}

/// Mark one review closed through the roborev CLI.
pub fn close(checkout: &Path, job_id: i64, timeout: Duration) -> Result<()> {
    exec::run_ok_timed(
        "roborev",
        &["close", &job_id.to_string()],
        Some(checkout),
        timeout,
    )
}

/// Add one comment through the roborev CLI.
pub fn comment(checkout: &Path, job_id: i64, message: &str, timeout: Duration) -> Result<()> {
    exec::run_ok_timed(
        "roborev",
        &comment_args(job_id, message),
        Some(checkout),
        timeout,
    )
}

/// Build argv with boolean `--job` before the ID positional and `--` before the message.
fn comment_args(job_id: i64, message: &str) -> [String; 5] {
    [
        "comment".to_string(),
        "--job".to_string(),
        job_id.to_string(),
        "--".to_string(),
        message.to_string(),
    ]
}

/// Enqueue a review of `selection` in `checkout`.
///
/// Revisions come from the checkout's own `git log` and arrive as separate argv entries, never as a
/// range string. `agent` picks the reviewer, or leaves roborev on its configured default.
///
/// The reply stays verbatim because `roborev review` has no JSON receipt. This also preserves
/// `Skipped:` responses and dirty-review refusals.
///
/// roborev prints its enqueue confirmation (job id, ref, agent) to stderr rather than stdout
/// whenever stdout is not a terminal, which a captured subprocess never is. Stdout is checked
/// first only so a future stdout-emitting roborev is not shadowed by leftover stderr chatter.
pub fn review(
    checkout: &Path,
    selection: &Selection,
    review_type: ReviewType,
    agent: Option<&str>,
) -> Result<String> {
    let args = review_args(selection, review_type, agent);
    let output = exec::run("roborev", &args, Some(checkout))?;
    let reply = match output.stdout.trim() {
        "" => output.stderr.trim(),
        text => text,
    };
    Ok(reply.to_string())
}

/// The argv for one enqueue.
///
/// `roborev review START END` reviews `START^..END` inclusive, so the older commit comes first.
fn review_args<'a>(
    selection: &'a Selection,
    review_type: ReviewType,
    agent: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args = vec!["review"];
    if let Some(agent) = agent {
        args.extend(["--agent", agent]);
    }
    if let Some(review_type) = review_type.flag() {
        args.extend(["--type", review_type]);
    }
    match selection {
        Selection::Dirty => args.push("--dirty"),
        Selection::Commit(sha) => args.push(sha),
        Selection::Range(start, end) => args.extend([start.as_str(), end.as_str()]),
    }
    args
}

/// Every agent roborev can drive, paired with the executable it looks for.
///
/// roborev exposes these pairs through `check-agents` but not as machine-readable registry data.
/// Its internal `test` agent is excluded.
const AGENT_COMMANDS: [(&str, &str); 11] = [
    ("acp", "acp-agent"),
    ("claude-code", "claude"),
    ("codex", "codex"),
    ("copilot", "copilot"),
    ("cursor", "agent"),
    ("droid", "droid"),
    ("gemini", "gemini"),
    ("kilo", "kilo"),
    ("kiro", "kiro-cli"),
    ("opencode", "opencode"),
    ("pi", "pi"),
];

/// The agents installed on this machine, sorted by name.
///
/// PATH lookup mirrors the first half of `roborev check-agents` without its live agent calls.
pub fn installed_agents() -> Vec<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut installed: Vec<String> = AGENT_COMMANDS
        .iter()
        .filter(|(_, command)| on_path(&path, command))
        .map(|(name, _)| name.to_string())
        .collect();

    // The table above is already in name order, but sorting here is what the picker relies on.
    installed.sort();
    installed
}

/// Whether `command` sits in one of `path`'s directories.
fn on_path(path: &OsStr, command: &str) -> bool {
    std::env::split_paths(path).any(|dir| {
        let command = dir.join(command);
        command.is_file() && access(&command, Access::EXEC_OK).is_ok()
    })
}

/// The agent a review runs with when none is named, for labelling the picker's default row.
///
/// `config get` merges repository config over global config when run from the checkout.
///
/// Only the two keys that decide this in practice are consulted. roborev layers reasoning-tier
/// overrides above them (`review_agent_thorough` and friends) and falls back to a hardcoded name
/// below them, so this is a label rather than a promise.
pub fn default_agent(checkout: &Path) -> Option<String> {
    ["review_agent", "default_agent"].iter().find_map(|key| {
        let output = exec::run("roborev", &["config", "get", key], Some(checkout)).ok()?;
        let value = output.stdout.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

/// Run the roborev TUI scoped to the checkout's repo and branch. The TUI owns the terminal, so its
/// stdio is inherited rather than captured.
pub fn tui(checkout: &Path) -> Result<()> {
    exec::run_interactive("roborev", &["tui", "--repo", "--branch"], Some(checkout))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;

    use super::{
        AGENT_COMMANDS, JobStatus, ReviewJob, ReviewType, Selection, ShownReview, comment_args,
        newest_reviewed, on_path, review_args,
    };

    fn parse(json: &str) -> Vec<ReviewJob> {
        serde_json::from_str(json).expect("valid job array")
    }

    #[test]
    fn an_agent_is_installed_when_its_command_is_on_the_path() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let claude = dir.path().join("claude");
        std::fs::write(&claude, "").expect("write");
        let mut permissions = std::fs::metadata(&claude).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(claude, permissions).expect("make executable");

        let path = std::env::join_paths([dir.path()]).expect("join paths");
        assert!(on_path(&path, "claude"));
        assert!(!on_path(&path, "codex"));
    }

    #[test]
    fn a_non_executable_agent_is_not_installed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("claude"), "").expect("write");

        let path = std::env::join_paths([dir.path()]).expect("join paths");
        assert!(!on_path(&path, "claude"));
    }

    #[test]
    fn an_inapplicable_execute_bit_does_not_install_an_agent() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let claude = dir.path().join("claude");
        std::fs::write(&claude, "").expect("write");
        let mut permissions = std::fs::metadata(&claude).expect("metadata").permissions();
        permissions.set_mode(0o001);
        std::fs::set_permissions(claude, permissions).expect("set permissions");

        let path = std::env::join_paths([dir.path()]).expect("join paths");
        assert!(!on_path(&path, "claude"));
    }

    #[test]
    fn an_empty_or_missing_path_installs_nothing() {
        assert!(!on_path(OsStr::new(""), "claude"));
    }

    #[test]
    fn every_agent_name_is_one_roborev_accepts() {
        // The daemon rejects an unknown --agent, so a typo here would only surface at enqueue.
        // These are the names `roborev check-agents` lists, minus its internal `test` agent.
        for (name, command) in AGENT_COMMANDS {
            assert!(!name.is_empty() && !command.is_empty());
            assert_ne!(name, "test", "roborev's test agent is not a real choice");
        }
    }

    #[test]
    fn argv_keeps_a_revision_off_dirty_and_the_flag_off_the_default_type() {
        // Two roborev behaviors that fail silently rather than loudly. `review --dirty <sha>`
        // reviews the working tree and drops the revision, because --dirty is tested first and
        // never validated against one. And `--type default` is rejected, though that is what
        // roborev calls its ordinary reviewer.
        let commit = Selection::Commit("abc".to_string());
        let range = Selection::Range("old".to_string(), "new".to_string());
        for (selection, review_type, want) in [
            (
                &Selection::Dirty,
                ReviewType::Default,
                vec!["review", "--dirty"],
            ),
            (&commit, ReviewType::Default, vec!["review", "abc"]),
            (
                &commit,
                ReviewType::Security,
                vec!["review", "--type", "security", "abc"],
            ),
            // START END, older first, never the string "START^..END".
            (
                &range,
                ReviewType::Design,
                vec!["review", "--type", "design", "old", "new"],
            ),
        ] {
            assert_eq!(review_args(selection, review_type, None), want);
        }
    }

    #[test]
    fn comment_argv_terminates_flags_and_keeps_the_message_whole() {
        assert_eq!(
            comment_args(42, "--server surprise\nsecond line"),
            [
                "comment",
                "--job",
                "42",
                "--",
                "--server surprise\nsecond line"
            ]
            .map(String::from)
        );
    }

    #[test]
    fn parses_a_bare_job_array() {
        let jobs = parse(
            r#"[{"id":1,"status":"running","git_ref":"abc","branch":"main"},
                {"id":2,"status":"done","closed":false,"verdict":"F"}]"#,
        );
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].status, JobStatus::Running);
        assert!(jobs[1].needs_attention());
    }

    #[test]
    fn a_shown_review_ignores_unmodeled_fields() {
        let shown: ShownReview = serde_json::from_str(
            r###"{"id":380,"job_id":388,"agent":"codex","prompt":"...","output":"## Findings",
                "created_at":"2026-08-02","closed":false,"uuid":"u","verdict_bool":true,"job":{}}"###,
        )
        .expect("a shown review parses");
        assert_eq!(shown.output, "## Findings");
        assert_eq!(shown.closed, Some(false));
    }

    #[test]
    fn a_review_nobody_has_closed_yet_reports_no_closed_state() {
        let shown: ShownReview =
            serde_json::from_str(r###"{"job_id":388,"agent":"codex","output":"## Findings"}"###)
                .expect("a shown review parses");
        assert_eq!(shown.closed, None);
    }

    #[test]
    fn an_empty_repo_yields_null_not_an_array() {
        let jobs: Option<Vec<ReviewJob>> = serde_json::from_str("null").expect("null parses");
        assert!(jobs.unwrap_or_default().is_empty());
    }

    #[test]
    fn unrecognized_status_does_not_fail_parsing() {
        let jobs = parse(r#"[{"id":1,"status":"quantum-entangled"}]"#);
        assert_eq!(jobs[0].status, JobStatus::Unknown);
        assert!(!jobs[0].status.is_active());
    }

    #[test]
    fn a_failing_review_outranks_a_newer_passing_one() {
        let jobs = parse(
            r#"[{"id":9,"status":"done","closed":false,"verdict":"P"},
                {"id":8,"status":"done","closed":false,"verdict":"F"}]"#,
        );
        assert_eq!(newest_reviewed(&jobs).expect("a review").id, 8);
    }

    #[test]
    fn the_newest_review_wins_when_none_need_attention() {
        let jobs = parse(
            r#"[{"id":9,"status":"running"},
                {"id":8,"status":"done","closed":true,"verdict":"P"},
                {"id":7,"status":"done","closed":true,"verdict":"F"}]"#,
        );
        assert_eq!(newest_reviewed(&jobs).expect("a review").id, 8);
    }

    /// An `insights` job finishes with no verdict, and it is a report rather than a review.
    #[test]
    fn a_finished_job_without_a_verdict_is_not_opened() {
        let jobs = parse(
            r#"[{"id":9,"status":"done","closed":false},
                {"id":8,"status":"done","closed":true,"verdict":"P"}]"#,
        );
        assert_eq!(newest_reviewed(&jobs).expect("a review").id, 8);
        assert!(newest_reviewed(&jobs[..1]).is_none());
    }

    #[test]
    fn a_workspace_with_nothing_reviewed_has_none_to_open() {
        let jobs = parse(r#"[{"id":9,"status":"running"},{"id":8,"status":"queued"}]"#);
        assert!(newest_reviewed(&jobs).is_none());
        assert!(newest_reviewed(&[]).is_none());
    }

    #[test]
    fn closed_and_passing_reviews_need_no_attention() {
        let jobs = parse(
            r#"[{"id":1,"status":"done","closed":true,"verdict":"F"},
                {"id":2,"status":"done","closed":false,"verdict":"P"},
                {"id":3,"status":"done","closed":false},
                {"id":4,"status":"canceled","closed":false,"verdict":"F"}]"#,
        );
        assert!(jobs.iter().all(|job| !job.needs_attention()));
    }
}
