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

    #[error("no finished review for this workspace yet")]
    NoFinishedReview,

    #[error("another reporter is already publishing review state")]
    ReporterAlreadyRunning,
}

pub type Result<T> = std::result::Result<T, Error>;
