//! The agents the commit picker offers, and the models each one runs with.

use std::fmt::Write as _;

use serde::Deserialize;

/// One agent the commit picker offers, and the models it can run with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentConfig {
    pub models: Vec<String>,
    /// Model sent when the agent is picked without choosing one. Absent leaves `--model` off, so
    /// roborev resolves it.
    #[serde(rename = "default")]
    pub default_model: Option<String>,
}

impl AgentConfig {
    /// The `default` naming no listed model, which would enqueue a review the agent rejects.
    pub fn unlisted_default(&self) -> Option<&str> {
        let model = self.default_model.as_deref()?;
        (!self.models.iter().any(|listed| listed == model)).then_some(model)
    }
}

/// Configured agents missing from PATH, and installed agents missing from the config.
pub fn drift(configured: &[String], installed: &[String]) -> (Vec<String>, Vec<String>) {
    let missing = |from: &[String], names: &[String]| -> Vec<String> {
        names
            .iter()
            .filter(|name| !from.contains(name))
            .cloned()
            .collect()
    };
    (
        missing(installed, configured),
        missing(configured, installed),
    )
}

/// The `[agents.<name>]` tables to add for `agents`, for a report to print rather than write.
///
/// Each arrives with no models, so pasting one changes nothing about how a review is enqueued
/// until a model is written beside it.
pub fn tables(agents: &[String]) -> String {
    let mut tables = String::new();
    for agent in agents {
        writeln!(tables, "[agents.{agent}]\nmodels = []\n")
            .expect("writing to a string cannot fail");
    }
    tables.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{drift, tables};
    use crate::config::Config;

    fn parse(raw: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(raw)
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn an_absent_table_and_an_empty_one_both_configure_nothing() {
        assert_eq!(parse("").expect("valid").agents, BTreeMap::new());
        assert_eq!(parse("[agents]").expect("valid").agents, BTreeMap::new());
    }

    #[test]
    fn an_agent_parses_its_models_and_default() {
        let config = parse(
            r#"
[agents.claude-code]
models = ["sonnet-5", "opus-5"]
default = "sonnet-5"

[agents.codex]
models = []
"#,
        )
        .expect("valid");

        let claude = &config.agents["claude-code"];
        assert_eq!(claude.models, ["sonnet-5", "opus-5"]);
        assert_eq!(claude.default_model.as_deref(), Some("sonnet-5"));
        assert_eq!(config.agents["codex"].default_model, None);
    }

    /// A default outside the list would enqueue a review the agent rejects, long after the typo.
    #[test]
    fn a_default_outside_the_models_list_is_named() {
        let config =
            parse("[agents.codex]\nmodels = [\"terra\"]\ndefault = \"sol\"\n").expect("parses");
        assert_eq!(config.agents["codex"].unlisted_default(), Some("sol"));
    }

    #[test]
    fn a_default_naming_a_listed_model_passes() {
        let config = parse("[agents.codex]\nmodels = [\"terra\", \"sol\"]\ndefault = \"sol\"\n")
            .expect("parses");
        assert_eq!(config.agents["codex"].unlisted_default(), None);
    }

    #[test]
    fn an_agent_without_a_default_has_nothing_to_check() {
        let config = parse("[agents.codex]\nmodels = [\"terra\"]\n").expect("parses");
        assert_eq!(config.agents["codex"].unlisted_default(), None);
    }

    #[test]
    fn an_unknown_agent_key_is_rejected() {
        let error =
            parse("[agents.codex]\nmodles = [\"terra\"]\n").expect_err("unknown key rejected");
        assert!(error.to_string().contains("modles"), "{error}");
    }

    /// Both halves come from one helper with swapped arguments, so a transposition compiles and
    /// silently trades the two lists for each other.
    #[test]
    fn drift_keeps_each_name_on_its_own_side() {
        let (uninstalled, unconfigured) = drift(
            &names(&["claude-code", "gemini"]),
            &names(&["claude-code", "codex"]),
        );
        assert_eq!(uninstalled, names(&["gemini"]));
        assert_eq!(unconfigured, names(&["codex"]));
    }

    #[test]
    fn an_agreeing_config_drifts_in_neither_direction() {
        let (uninstalled, unconfigured) = drift(&names(&["codex"]), &names(&["codex"]));
        assert!(uninstalled.is_empty());
        assert!(unconfigured.is_empty());
    }

    /// What the report prints has to be pasteable as it stands.
    #[test]
    fn the_printed_tables_parse_as_a_config() {
        let tables = tables(&names(&["claude-code", "codex"]));
        let config = parse(&tables).expect("the printed tables parse");

        assert_eq!(config.agents.len(), 2);
        assert!(config.agents["codex"].models.is_empty());
        assert_eq!(config.agents["claude-code"].default_model, None);
        assert!(!tables.ends_with('\n'));
    }
}
