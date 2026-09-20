use std::path::PathBuf;

/// Application-wide error type for the roboherd plugin binary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("failed to spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{program} exited with {code}: {stderr}")]
    CommandFailed {
        program: String,
        code: String,
        stderr: String,
    },

    #[error("{program} produced invalid UTF-8 output")]
    CommandUtf8 { program: String },

    #[error("failed to read {program} output: {source}")]
    CommandOutput {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {program} input: {source}")]
    CommandInput {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{program} did not finish within {seconds}s")]
    CommandTimeout { program: String, seconds: u64 },

    #[error("failed to parse {program} JSON output: {source}")]
    CommandJson {
        program: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("HERDR_PLUGIN_CONTEXT_JSON is not valid JSON: {source}")]
    ContextJson {
        #[source]
        source: serde_json::Error,
    },

    #[error("no workspace checkout resolved from plugin context at {0}")]
    NoCheckout(PathBuf),

    #[error("{path} is not valid roboherd configuration: {source}")]
    ConfigToml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to read {path}: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: agents.{agent} defaults to {model}, which is not in its models list")]
    ConfigAgentDefault {
        path: PathBuf,
        agent: String,
        model: String,
    },

    #[error("no finished review for this workspace yet")]
    NoFinishedReview,

    #[error("another reporter is already publishing review state")]
    ReporterAlreadyRunning,
}

/// Longest toast body, past which the notification spans the screen it interrupts.
const MAX_SUMMARY: usize = 90;

impl Error {
    /// One short line naming what broke and where, for a toast.
    ///
    /// A TOML error's display spends a full path on a file there is one of and draws a caret
    /// diagram a toast cannot align, so its `message` is used instead. `CommandFailed` embeds a
    /// subprocess's stderr, so any variant can still arrive with line breaks in it. stderr and the
    /// plugin log keep the whole thing.
    pub fn summary(&self) -> String {
        let summary = match self {
            Error::ConfigToml { path, source } => {
                let file = path.file_name().unwrap_or(path.as_os_str());
                format!("{}: {}", file.to_string_lossy(), source.message())
            }
            other => other.to_string(),
        };
        let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
        match summary.char_indices().nth(MAX_SUMMARY) {
            Some((end, _)) => format!("{}\u{2026}", &summary[..end]),
            None => summary,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Error;

    fn config_error(raw: &str) -> Error {
        let source = toml::from_str::<toml::Table>(raw).expect_err("invalid");
        Error::ConfigToml {
            path: PathBuf::from("/Users/someone/.config/herdr/plugins/config/roboherd/config.toml"),
            source,
        }
    }

    /// The toast has to say what broke and which file it is in, without the path that made it span
    /// the screen or the caret diagram that cannot align in it.
    #[test]
    fn a_toml_error_names_its_file_and_its_cause() {
        let summary = config_error("tui_placement = \"popup\"\n\n[test]\nkey = ?\n").summary();

        assert!(summary.starts_with("config.toml: "), "{summary}");
        assert!(!summary.contains("/Users/someone"), "{summary}");
        assert!(!summary.contains("^^^"), "{summary}");
        assert!(!summary.contains('\n'), "{summary}");
    }

    /// `CommandFailed` carries a subprocess's stderr, which arrives with its own line breaks.
    #[test]
    fn an_embedded_stderr_is_flattened_to_one_line() {
        let summary = Error::CommandFailed {
            program: "roborev".to_string(),
            code: "1".to_string(),
            stderr: "first line\n\nsecond line\n  third line".to_string(),
        }
        .summary();

        assert_eq!(
            summary,
            "roborev exited with 1: first line second line third line"
        );
    }

    #[test]
    fn an_already_short_error_is_passed_through() {
        assert_eq!(
            Error::NoFinishedReview.summary(),
            "no finished review for this workspace yet"
        );
    }

    #[test]
    fn an_overlong_summary_is_cut_short() {
        let summary = Error::CommandUtf8 {
            program: "x".repeat(500),
        }
        .summary();

        assert_eq!(summary.chars().count(), 91);
        assert!(summary.ends_with('\u{2026}'));
    }
}
