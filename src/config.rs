//! roboherd's own settings, read from the config directory herdr hands every plugin.

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Environment variable carrying the plugin's config directory.
const CONFIG_DIR_ENV: &str = "HERDR_PLUGIN_CONFIG_DIR";

/// The config file inside that directory.
const CONFIG_FILE: &str = "config.toml";

/// Where the roborev TUI opens. A popup never reaches a pane listing, so only a tab can toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuiPlacement {
    #[default]
    Popup,
    Tab,
}

/// Every roboherd setting. An unknown key is rejected rather than ignored, so a typo surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub tui_placement: TuiPlacement,
}

impl Config {
    /// Read the config file, falling back to defaults when it does not exist.
    ///
    /// Herdr sets [`CONFIG_DIR_ENV`] for every action, so an unset variable means a hand-run binary.
    pub fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => return Err(Error::ConfigRead { path, source }),
        };

        toml::from_str(&raw).map_err(|source| Error::ConfigToml { path, source })
    }

    /// The config file path, absent when herdr named no config directory.
    fn path() -> Option<PathBuf> {
        std::env::var_os(CONFIG_DIR_ENV)
            .map(PathBuf::from)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(CONFIG_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, TuiPlacement};

    fn parse(raw: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(raw)
    }

    #[test]
    fn an_empty_file_leaves_every_default() {
        assert_eq!(parse("").expect("valid"), Config::default());
        assert_eq!(Config::default().tui_placement, TuiPlacement::Popup);
    }

    #[test]
    fn each_placement_parses() {
        assert_eq!(
            parse(r#"tui_placement = "tab""#)
                .expect("valid")
                .tui_placement,
            TuiPlacement::Tab
        );
        assert_eq!(
            parse(r#"tui_placement = "popup""#)
                .expect("valid")
                .tui_placement,
            TuiPlacement::Popup
        );
    }

    /// A typo must not read as "leave the default", which is what ignoring it would look like.
    #[test]
    fn an_unknown_key_is_rejected() {
        let error = parse(r#"tui_placment = "tab""#).expect_err("unknown key rejected");
        assert!(error.to_string().contains("tui_placment"), "{error}");
    }

    #[test]
    fn an_unsupported_placement_names_the_ones_that_work() {
        let error = parse(r#"tui_placement = "overlay""#).expect_err("bad value rejected");
        let message = error.to_string();
        assert!(
            message.contains("popup") && message.contains("tab"),
            "{message}"
        );
    }
}
