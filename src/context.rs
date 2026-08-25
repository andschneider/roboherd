use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::git;

/// Environment variable carrying the full plugin invocation context.
const CONTEXT_ENV: &str = "HERDR_PLUGIN_CONTEXT_JSON";

/// How long [`Context::existing_checkout`] gets to resolve the repo root.
const ROOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Git checkout provenance for a workspace opened from a worktree. The checkout path keeps git
/// commands on the worktree's branch even when the workspace cwd names the main checkout.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Worktree {
    #[serde(default)]
    pub checkout_path: String,
}

/// The subset of herdr's plugin invocation context that roboherd reads. Unknown fields are ignored
/// so a newer herdr can add context without breaking the plugin.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Context {
    /// The workspace an action fired from, used to scope a pane listing when locating a pane.
    pub workspace_id: Option<String>,
    pub workspace_cwd: Option<String>,
    pub worktree: Option<Worktree>,
    /// The pane an action fired from, and the pane a split divides.
    pub focused_pane_id: Option<String>,
}

impl Context {
    /// Read and parse the plugin context. An unset variable yields an empty context, since startup
    /// hooks run without workspace scope.
    pub fn from_env() -> Result<Self> {
        match std::env::var(CONTEXT_ENV) {
            Ok(raw) if !raw.trim().is_empty() => Self::parse(&raw),
            _ => Ok(Self::default()),
        }
    }

    /// Parse a context JSON document.
    pub fn parse(raw: &str) -> Result<Self> {
        serde_json::from_str(raw).map_err(|source| Error::ContextJson { source })
    }

    /// The checkout that roborev and git commands should run in. A worktree path is authoritative,
    /// then an ordinary workspace falls back to its cwd.
    pub fn checkout(&self) -> PathBuf {
        self.worktree
            .as_ref()
            .and_then(|w| non_empty(Some(&w.checkout_path)))
            .or_else(|| non_empty(self.workspace_cwd.as_deref()))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The checkout, rejected when it does not exist on disk and normalized to the repo root.
    ///
    /// Roborev resolves its own `--repo` from `.` and gets a relative `..` below the root, which
    /// matches no stored job. A path outside a repository passes through for roborev to reject.
    pub fn existing_checkout(&self) -> Result<PathBuf> {
        let path = self.checkout();
        if !path.is_dir() {
            return Err(Error::NoCheckout(path));
        }
        Ok(git::repo_root(&path, ROOT_TIMEOUT).unwrap_or(path))
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// The pane a split should divide, preferring the context and falling back to the env var herdr
/// sets for keybound commands.
pub fn focused_pane_id(context: &Context) -> Option<String> {
    context
        .focused_pane_id
        .clone()
        .or_else(|| std::env::var("HERDR_ACTIVE_PANE_ID").ok())
        .filter(|id| !id.trim().is_empty())
}

/// Path to the herdr binary, honoring the portable path herdr exports to plugins.
pub fn herdr_bin() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{Context, Worktree};
    use crate::git;
    use crate::git::fixtures::{git as run_git, repo_with_one_commit};

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn context_at(path: &std::path::Path) -> Context {
        Context {
            workspace_cwd: Some(path.display().to_string()),
            ..Context::default()
        }
    }

    /// Roborev resolves its own `--repo` from `.` and gets it wrong below the root, so a
    /// subdirectory must not reach it.
    #[test]
    fn a_subdirectory_checkout_is_normalized_to_the_repo_root() {
        let repo = repo_with_one_commit();
        let nested = repo.path().join("src/reporter");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        assert_eq!(
            context_at(&nested)
                .existing_checkout()
                .expect("checkout resolves"),
            repo.path().canonicalize().expect("canonical repo root")
        );
    }

    #[test]
    fn a_checkout_outside_a_repository_is_left_alone() {
        let dir = TempDir::new().expect("tempdir");
        assert_eq!(
            context_at(dir.path())
                .existing_checkout()
                .expect("checkout resolves"),
            dir.path()
        );
    }

    #[test]
    fn worktree_checkout_wins_over_workspace_cwd() {
        let ctx = Context::parse(
            r#"{"workspace_cwd":"/a","worktree":{"checkout_path":"/b","repo_name":"r",
                "repo_root":"/b","is_linked_worktree":true}}"#,
        )
        .expect("valid context");
        assert_eq!(ctx.checkout().to_str(), Some("/b"));
    }

    #[test]
    fn worktree_checkout_keeps_git_on_its_branch() {
        let repo = repo_with_one_commit();
        let parent = TempDir::new().expect("tempdir");
        let linked = parent.path().join("linked");
        run_git(
            repo.path(),
            &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
            None,
        );
        let ctx = Context {
            workspace_cwd: Some(repo.path().display().to_string()),
            worktree: Some(Worktree {
                checkout_path: linked.display().to_string(),
            }),
            ..Context::default()
        };

        let checkout = ctx.existing_checkout().expect("checkout resolves");
        assert_eq!(
            checkout,
            linked.canonicalize().expect("canonical worktree path")
        );
        assert_eq!(git::current_branch(&checkout, TIMEOUT).unwrap(), "feature");
    }

    #[test]
    fn blank_worktree_checkout_falls_back_to_workspace_cwd() {
        let ctx = Context::parse(
            r#"{"workspace_cwd":"/a","worktree":{"checkout_path":"   ","repo_name":"r",
                "repo_root":"/b","is_linked_worktree":true}}"#,
        )
        .expect("valid context");
        assert_eq!(ctx.checkout().to_str(), Some("/a"));
    }

    #[test]
    fn falls_back_to_current_directory() {
        let ctx = Context::parse("{}").expect("valid context");
        assert_eq!(ctx.checkout().to_str(), Some("."));
    }

    #[test]
    fn blank_workspace_cwd_is_skipped() {
        let ctx = Context::parse(r#"{"workspace_cwd":"   "}"#).expect("valid context");
        assert_eq!(ctx.checkout().to_str(), Some("."));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let ctx = Context::parse(r#"{"workspace_cwd":"/a","future_field":42}"#)
            .expect("unknown fields tolerated");
        assert_eq!(ctx.checkout().to_str(), Some("/a"));
    }
}
