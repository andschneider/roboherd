use std::time::Duration;

use semver::Version;

use crate::context;
use crate::exec;

pub const MIN_HERDR_VERSION: &str = "0.7.5";
pub const MIN_ROBOREV_VERSION: &str = "0.63.0";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One external tool result ready for display.
pub struct Check {
    pub level: Level,
    pub message: String,
}

/// Warn about external tool versions that cannot be confirmed as supported.
pub fn warn() {
    for check in inspect() {
        if check.level != Level::Ok {
            eprintln!("roboherd: warning: {}", check.message);
        }
    }
}

/// Inspect every external tool without stopping at the first problem.
pub fn inspect() -> [Check; 2] {
    [
        inspect_tool(
            "herdr",
            MIN_HERDR_VERSION,
            &context::herdr_bin(),
            &["--version"],
        ),
        inspect_tool("roborev", MIN_ROBOREV_VERSION, "roborev", &["version"]),
    ]
}

fn inspect_tool(tool: &'static str, minimum: &'static str, program: &str, args: &[&str]) -> Check {
    let (level, message) = match exec::run_timed(program, args, None, VERSION_TIMEOUT) {
        Ok(output) => match output.stdout.split_whitespace().find_map(parse_version) {
            Some(found) if version_level(&found, minimum) == Level::Ok => {
                (Level::Ok, format!("{tool} {found}"))
            }
            Some(found) => (
                Level::Fail,
                format!("{tool} {found} requires {minimum} or newer"),
            ),
            None => (
                Level::Warn,
                format!(
                    "{tool} version could not be verified: {}",
                    output.stdout.trim()
                ),
            ),
        },
        Err(err) => (
            Level::Warn,
            format!("{tool} version could not be verified: {err}"),
        ),
    };
    Check { level, message }
}

fn version_level(found: &Version, minimum: &str) -> Level {
    let minimum = Version::parse(minimum).expect("minimum version must be semver");
    if found < &minimum {
        Level::Fail
    } else {
        Level::Ok
    }
}

fn parse_version(word: &str) -> Option<Version> {
    let version = word.trim_start_matches('v');
    let version = version.strip_suffix("-dirty").unwrap_or(version);
    Version::parse(version).ok()
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{Level, MIN_HERDR_VERSION, MIN_ROBOREV_VERSION, parse_version, version_level};

    #[test]
    fn prereleases_are_classified_below_the_release() {
        let prerelease = Version::parse("0.63.0-rc.1").unwrap();
        assert!(version_level(&prerelease, MIN_ROBOREV_VERSION) == Level::Fail);
        let release = Version::parse("0.63.0").unwrap();
        assert!(version_level(&release, MIN_ROBOREV_VERSION) == Level::Ok);
    }

    #[test]
    fn dirty_release_versions_use_their_release_tag() {
        assert_eq!(
            parse_version("v0.63.0-dirty").unwrap(),
            Version::new(0, 63, 0)
        );
        assert!(parse_version("abcdef1-dirty").is_none());
    }

    #[test]
    fn manifest_and_readme_match_runtime_requirements() {
        let manifest: toml::Value = toml::from_str(include_str!("../herdr-plugin.toml")).unwrap();
        assert_eq!(
            manifest["min_herdr_version"].as_str(),
            Some(MIN_HERDR_VERSION)
        );

        let readme = include_str!("../README.md");
        assert!(readme.contains(&format!("**herdr ≥ {MIN_HERDR_VERSION}**")));
        assert!(readme.contains(&format!("**roborev ≥ {MIN_ROBOREV_VERSION}**")));
    }
}
