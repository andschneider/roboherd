use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::exec;

/// One row of `git log` shown in the commit picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    /// Author date as a unix timestamp.
    ///
    /// The picker derives its fixed-width age instead of using git's localized `%ar` prose.
    pub timestamp: i64,
    pub subject: String,
}

/// Field separator for the picker's `git log` format. A unit separator cannot appear in a commit
/// subject, unlike tabs or pipes.
const FIELD_SEP: char = '\u{1f}';

/// Recent commits reachable from HEAD in `checkout`, newest first.
///
/// An unborn HEAD yields an empty list. `git log` exits 128 for an unborn HEAD, a non-repository,
/// and a broken repository, so `rev-list --all` confirms that the repository remains readable.
pub fn recent_commits(checkout: &Path, limit: usize) -> Result<Vec<Commit>> {
    let format = format!("--format=%H{FIELD_SEP}%h{FIELD_SEP}%an{FIELD_SEP}%at{FIELD_SEP}%s");
    // Author names and subjects are raw bytes to git. Only the shas and timestamp are parsed, so
    // decoding loosely costs one replacement character rather than the whole picker.
    let output = match exec::run_lossy(
        "git",
        &["log", &format!("--max-count={limit}"), &format],
        Some(checkout),
    ) {
        Ok(output) => output,
        // Asking git to tell the two apart costs a process only on the path that already failed.
        Err(Error::CommandFailed { .. }) if is_readable_repository(checkout) => {
            return Ok(Vec::new());
        }
        Err(other) => return Err(other),
    };

    Ok(output.stdout.lines().filter_map(parse_log_line).collect())
}

/// How many paths in `checkout` carry uncommitted changes, staged, unstaged, or untracked.
///
/// The porcelain listing matches roborev's input before exclude patterns, so this count is an upper
/// bound. An unborn HEAD counts changes against the empty tree.
pub fn dirty_count(checkout: &Path) -> Result<usize> {
    let output = exec::run(
        "git",
        &["status", "--porcelain=v1", "-uall"],
        Some(checkout),
    )?;
    Ok(output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

/// The commits in the inclusive range between `start` and `end`, as full SHAs.
///
/// Picker rows use date order, but `START^..END` uses ancestry. Resolving the range lets the picker
/// verify that every highlighted commit is included.
pub fn range_commits(checkout: &Path, start: &str, end: &str) -> Result<Vec<String>> {
    let range = format!("{start}^..{end}");
    let output = exec::run("git", &["rev-list", &range], Some(checkout))?;
    Ok(output.stdout.lines().map(str::to_string).collect())
}

/// Whether `revision` names a commit with no parent: the repository's root.
///
/// `START^..END` cannot begin at a root because `START^` does not exist. Other failures remain
/// errors.
///
/// Reading the commit object distinguishes a root from a missing object because `rev-parse` accepts
/// a full SHA without reading the object database. A shallow boundary returns `false` because its
/// stored parent header remains present even when the parent object is unavailable.
pub fn is_root_commit(checkout: &Path, revision: &str) -> Result<bool> {
    let output = exec::run("git", &["cat-file", "commit", revision], Some(checkout))?;

    // Headers run to the first empty line. A message body can hold a line beginning "parent ", and
    // a signature's continuation lines are indented rather than empty, so neither ends the scan
    // early or is mistaken for a header.
    Ok(!output
        .stdout
        .lines()
        .take_while(|line| !line.is_empty())
        .any(|line| line.starts_with("parent ")))
}

/// Whether `rev-list --all` can traverse the repository's refs and objects.
///
/// This succeeds for an unborn HEAD even when another ref contains commits. It fails outside a
/// repository or when an existing ref names a missing object.
fn is_readable_repository(checkout: &Path) -> bool {
    exec::run("git", &["rev-list", "--all", "--count"], Some(checkout)).is_ok()
}

/// The repository root containing `path`, or `None` when it is not inside a checkout. A pane can
/// sit in a subdirectory, so its cwd is normalized before roborev runs there.
///
/// This runs on the reporter's poll path, where a pane parked on an unresponsive network mount
/// would otherwise stall the pass, so it is bounded by `timeout` like the other polled commands.
pub fn repo_root(path: &Path, timeout: Duration) -> Option<PathBuf> {
    let output = exec::run_timed(
        "git",
        &["rev-parse", "--show-toplevel"],
        Some(path),
        timeout,
    )
    .ok()?;
    let root = output.stdout.trim();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// The Git directory shared by every worktree of the repository containing `path`.
pub fn common_dir(path: &Path, timeout: Duration) -> Result<PathBuf> {
    let output = exec::run_timed(
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        Some(path),
        timeout,
    )?;
    Ok(PathBuf::from(output.stdout.trim()))
}

/// The checked-out branch name, or an empty string for detached HEAD.
///
/// This runs on the reporter's poll path so the tracker can keep branch histories separate.
pub fn current_branch(checkout: &Path, timeout: Duration) -> Result<String> {
    let output = exec::run_timed(
        "git",
        &["branch", "--show-current"],
        Some(checkout),
        timeout,
    )?;
    Ok(output.stdout.trim().to_string())
}

fn parse_log_line(line: &str) -> Option<Commit> {
    let mut fields = line.splitn(5, FIELD_SEP);
    Some(Commit {
        sha: fields.next()?.to_string(),
        short_sha: fields.next()?.to_string(),
        author: fields.next()?.to_string(),
        timestamp: fields.next()?.parse().ok()?,
        subject: fields.next().unwrap_or_default().to_string(),
    })
}

/// Throwaway repositories for tests. The picker's range handling is only meaningful against real
/// git history, so `src/panes/commit_picker.rs` builds its fixtures from here too.
#[cfg(test)]
pub mod fixtures {
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    /// Run git in `dir`, pinning the commit date when one is given so log order does not depend on
    /// how fast the test runs.
    pub fn git(dir: &Path, args: &[&str], date: Option<&str>) {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com");

        if let Some(date) = date {
            command
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date);
        }

        let status = command.output().expect("git runs");
        assert!(status.status.success(), "git {args:?} failed");
    }

    /// Commit a file named `name` on `day` of January 2026.
    pub fn commit(dir: &Path, name: &str, day: u32) {
        std::fs::write(dir.join(name), name).expect("write");
        git(dir, &["add", "."], None);
        git(
            dir,
            &["commit", "-m", name],
            Some(&format!("2026-01-{day:02}T12:00:00")),
        );
    }

    pub fn repo_with_one_commit() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        git(dir.path(), &["init", "--initial-branch=main"], None);
        commit(dir.path(), "first commit", 1);
        dir
    }

    /// A repo whose side branch interleaves with the mainline by date:
    ///
    /// ```text
    /// merge  day 5
    /// main2  day 4
    /// side1  day 3   <- on a side branch, not an ancestor of main2
    /// main1  day 2
    /// base   day 1
    /// ```
    pub fn repo_with_an_interleaved_branch() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        git(path, &["init", "--initial-branch=main"], None);

        commit(path, "base", 1);
        git(path, &["checkout", "-b", "side"], None);
        commit(path, "side1", 3);
        git(path, &["checkout", "main"], None);
        commit(path, "main1", 2);
        commit(path, "main2", 4);
        git(
            path,
            &["merge", "--no-ff", "side", "-m", "merge"],
            Some("2026-01-05T12:00:00"),
        );
        dir
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::fixtures::{git, repo_with_an_interleaved_branch, repo_with_one_commit};
    use super::{
        Commit, FIELD_SEP, common_dir, current_branch, is_root_commit, parse_log_line,
        range_commits, recent_commits, repo_root,
    };

    /// Generous enough that a slow test machine never trips it.
    const TIMEOUT: Duration = Duration::from_secs(30);

    fn subjects(commits: &[Commit]) -> Vec<&str> {
        commits.iter().map(|c| c.subject.as_str()).collect()
    }

    #[test]
    fn parses_a_log_line() {
        let line = format!(
            "{0}{sep}{1}{sep}Ada{sep}1754006400{sep}subject with{sep}separator",
            "f".repeat(40),
            "fffffff",
            sep = FIELD_SEP
        );
        let commit = parse_log_line(&line).expect("parsed");
        assert_eq!(commit.author, "Ada");
        assert_eq!(commit.timestamp, 1_754_006_400);
        assert_eq!(commit.subject, format!("subject with{FIELD_SEP}separator"));
    }

    #[test]
    fn a_line_without_a_numeric_timestamp_is_dropped() {
        let line = format!(
            "{0}{sep}{1}{sep}Ada{sep}2 days ago{sep}subject",
            "f".repeat(40),
            "fffffff",
            sep = FIELD_SEP
        );
        assert!(parse_log_line(&line).is_none());
    }

    #[test]
    fn a_logged_commit_carries_a_plausible_timestamp() {
        let dir = repo_with_one_commit();
        let commits = recent_commits(dir.path(), 10).expect("log runs");
        // The fixture pins its dates, so this only rules out a zero or a parse that landed on
        // another field.
        assert!(
            commits[0].timestamp > 1_577_836_800,
            "timestamp {} predates 2020",
            commits[0].timestamp
        );
    }

    #[test]
    fn lists_recent_commits() {
        let dir = repo_with_one_commit();
        let commits = recent_commits(dir.path(), 10).expect("log runs");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "first commit");
        assert_eq!(commits[0].sha.len(), 40);
    }

    #[test]
    fn reads_the_checked_out_branch() {
        let dir = repo_with_one_commit();

        assert_eq!(current_branch(dir.path(), TIMEOUT).unwrap(), "main");
    }

    #[test]
    fn detached_head_has_no_branch() {
        let dir = repo_with_one_commit();
        git(dir.path(), &["checkout", "--detach"], None);

        assert_eq!(current_branch(dir.path(), TIMEOUT).unwrap(), "");
    }

    #[test]
    fn linked_worktrees_share_a_common_directory() {
        let dir = repo_with_one_commit();
        let parent = TempDir::new().expect("tempdir");
        let linked = parent.path().join("linked");
        git(
            dir.path(),
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
            None,
        );

        assert_ne!(repo_root(dir.path(), TIMEOUT), repo_root(&linked, TIMEOUT));
        assert_eq!(
            common_dir(dir.path(), TIMEOUT).unwrap(),
            common_dir(&linked, TIMEOUT).unwrap()
        );
    }

    #[test]
    fn a_date_ordered_span_is_not_an_ancestry_range() {
        let dir = repo_with_an_interleaved_branch();
        let commits = recent_commits(dir.path(), 10).expect("log runs");
        assert_eq!(
            subjects(&commits),
            ["merge", "main2", "side1", "main1", "base"]
        );

        // Marking rows 1 through 3 covers main2, side1, and main1 on screen. The range those rows
        // imply holds only two commits, because side1 is not reachable from main2.
        let ranged =
            range_commits(dir.path(), &commits[3].sha, &commits[1].sha).expect("rev-list runs");
        assert_eq!(ranged.len(), 2, "{ranged:?}");
        assert!(
            !ranged.contains(&commits[2].sha),
            "side1 was marked but the range does not review it"
        );
    }

    #[test]
    fn an_ancestry_range_holds_exactly_its_span() {
        let dir = repo_with_an_interleaved_branch();
        let commits = recent_commits(dir.path(), 10).expect("log runs");

        // main1 is an ancestor of the merge, and everything between them on screen is reachable.
        let ranged =
            range_commits(dir.path(), &commits[3].sha, &commits[0].sha).expect("rev-list runs");
        assert_eq!(ranged.len(), 4, "{ranged:?}");
    }

    #[test]
    fn a_range_beginning_at_the_root_commit_has_no_parent() {
        let dir = repo_with_one_commit();
        let commits = recent_commits(dir.path(), 10).expect("log runs");
        assert!(
            range_commits(dir.path(), &commits[0].sha, &commits[0].sha).is_err(),
            "the root commit has no parent to start a range from"
        );
    }

    /// Delete the loose object backing `sha`, leaving the rest of the store intact.
    fn drop_object(checkout: &Path, sha: &str) {
        let (dir, file) = sha.split_at(2);
        let path = checkout.join(".git/objects").join(dir).join(file);
        std::fs::remove_file(&path).unwrap_or_else(|err| panic!("removing {path:?}: {err}"));
    }

    #[test]
    fn only_the_oldest_commit_is_the_root() {
        let dir = repo_with_an_interleaved_branch();
        let commits = recent_commits(dir.path(), 10).expect("log runs");

        let root = commits.last().expect("a root commit");
        assert!(is_root_commit(dir.path(), &root.sha).expect("cat-file runs"));
        assert!(!is_root_commit(dir.path(), &commits[0].sha).expect("cat-file runs"));
    }

    #[test]
    fn a_revision_that_does_not_resolve_is_an_error_not_a_root() {
        let dir = repo_with_one_commit();
        assert!(is_root_commit(dir.path(), "not-a-ref").is_err());
    }

    #[test]
    fn a_missing_commit_object_is_not_mistaken_for_the_root() {
        // `rev-parse --verify` accepts a full SHA without reading the object, so a commit whose
        // object is gone verifies while its parent does not. Only reading the object catches it.
        let dir = repo_with_an_interleaved_branch();
        let commits = recent_commits(dir.path(), 10).expect("log runs");

        drop_object(dir.path(), &commits[0].sha);
        assert!(is_root_commit(dir.path(), &commits[0].sha).is_err());
    }

    #[test]
    fn a_parent_line_in_a_commit_message_is_not_read_as_a_header() {
        let dir = TempDir::new().expect("tempdir");
        git(dir.path(), &["init", "--initial-branch=main"], None);
        std::fs::write(dir.path().join("a"), "a").expect("write");
        git(dir.path(), &["add", "."], None);
        git(
            dir.path(),
            &[
                "commit",
                "-m",
                "root",
                "-m",
                "parent 0000000000000000000000000000000000000000",
            ],
            Some("2026-01-01T12:00:00"),
        );

        let commits = recent_commits(dir.path(), 10).expect("log runs");
        assert!(
            is_root_commit(dir.path(), &commits[0].sha).expect("cat-file runs"),
            "a message body was read as a commit header"
        );
    }

    #[test]
    fn a_broken_repository_is_not_reported_as_an_empty_one() {
        // `git log` fails for a repository missing its objects exactly as it does for one with no
        // commits yet. Reporting the first as empty would hide corruption behind a blank picker.
        let dir = repo_with_one_commit();
        let commits = recent_commits(dir.path(), 10).expect("log runs");

        drop_object(dir.path(), &commits[0].sha);
        assert!(recent_commits(dir.path(), 10).is_err());
    }

    #[test]
    fn a_checkout_with_no_commits_yet_lists_nothing() {
        let dir = TempDir::new().expect("tempdir");
        git(dir.path(), &["init", "--initial-branch=main"], None);
        let commits = recent_commits(dir.path(), 10).expect("an unborn HEAD is not an error");
        assert!(commits.is_empty());
    }

    /// `git commit` normalizes an author name to UTF-8, so the object is written directly here.
    /// The reachable case is a commit made by another tool and fetched in.
    #[test]
    fn a_commit_whose_author_is_not_utf8_still_lists() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let dir = TempDir::new().expect("tempdir");
        git(dir.path(), &["init", "--initial-branch=main"], None);
        std::fs::write(dir.path().join("a"), "a").expect("write");
        git(dir.path(), &["add", "."], None);

        let tree = Command::new("git")
            .args(["write-tree"])
            .current_dir(dir.path())
            .output()
            .expect("write-tree runs");
        let tree = String::from_utf8(tree.stdout).expect("a tree sha is ascii");

        let mut object = format!("tree {}\n", tree.trim()).into_bytes();
        object.extend_from_slice(b"author Ada\xffLovelace <t@e.com> 1767268800 +0000\n");
        object.extend_from_slice(b"committer T <t@e.com> 1767268800 +0000\n\nsubject\n");

        let mut hash = Command::new("git")
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("hash-object runs");
        hash.stdin
            .take()
            .expect("stdin")
            .write_all(&object)
            .expect("write the object");
        let sha = hash.wait_with_output().expect("hash-object finishes");
        let sha = String::from_utf8(sha.stdout).expect("a commit sha is ascii");
        git(
            dir.path(),
            &["update-ref", "refs/heads/main", sha.trim()],
            None,
        );

        let commits = recent_commits(dir.path(), 10).expect("an invalid author name is not fatal");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "subject");
        assert!(
            commits[0].author.contains('\u{fffd}'),
            "author {:?} never reached the decoder",
            commits[0].author
        );
    }

    #[test]
    fn a_path_outside_any_repo_is_still_an_error() {
        let dir = TempDir::new().expect("tempdir");
        assert!(recent_commits(dir.path(), 10).is_err());
    }

    #[test]
    fn a_subdirectory_normalizes_to_the_repo_root() {
        let dir = repo_with_one_commit();
        let nested = dir.path().join("deep/nested");
        std::fs::create_dir_all(&nested).expect("create nested dirs");

        // git resolves symlinks in its answer, and macOS temp dirs are symlinked, so compare
        // against the canonical path rather than the tempdir path as handed out.
        let expected = dir.path().canonicalize().expect("canonical repo path");
        let root = repo_root(&nested, TIMEOUT).expect("nested path is inside the repo");
        assert_eq!(root, expected);
    }

    #[test]
    fn a_path_outside_any_repo_has_no_root() {
        let dir = TempDir::new().expect("tempdir");
        assert_eq!(repo_root(dir.path(), TIMEOUT), None);
    }
}
