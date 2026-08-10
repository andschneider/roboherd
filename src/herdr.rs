use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::context;
use crate::error::Result;
use crate::exec;

/// The metadata source id roboherd reports under. Herdr scopes token ownership by source, so this
/// is the name that owns clearing and TTL refresh for `$roborev`.
pub const SOURCE: &str = "roborev";

/// The sidebar token names, referenced as `$roborev_f`, `$roborev_p`, and `$roborev_r` in
/// `[ui.sidebar.spaces].rows`, in the order a row places them.
///
/// Separate tokens allow each count to carry its own `fg` because herdr styles a token as a whole.
/// Herdr inserts its fixed dim ` · ` between adjacent tokens.
pub const TOKENS: [&str; 3] = ["roborev_f", "roborev_p", "roborev_r"];

/// Git checkout provenance attached to a workspace opened from a worktree.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeInfo {
    pub checkout_path: String,
}

/// One open herdr workspace.
#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
    pub active_tab_id: String,
    #[serde(default)]
    pub worktree: Option<WorktreeInfo>,
}

/// One pane, read for its cwd. Only worktree workspaces carry a checkout path of their own, so a
/// pane's cwd is the only path source for an ordinary workspace.
#[derive(Debug, Clone, Deserialize)]
pub struct Pane {
    /// Absent on a pane herdr has no working directory for, which it omits rather than nulls.
    #[serde(default)]
    pub cwd: Option<String>,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub pane_id: String,
    /// The manifest `title` of the plugin pane running here, absent for ordinary shell panes.
    #[serde(default)]
    pub label: Option<String>,
}

/// The workspaces and panes of one `api snapshot`, all taken at the same instant.
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub workspaces: Vec<Workspace>,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Deserialize)]
struct PaneListResponse {
    result: PaneListResult,
}

#[derive(Debug, Deserialize)]
struct PaneListResult {
    panes: Vec<Pane>,
}

#[derive(Debug, Deserialize)]
struct SnapshotResponse {
    result: SnapshotResult,
}

/// The snapshot envelope nests one level deeper than the list responses.
#[derive(Debug, Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}

impl Workspace {
    /// The worktree checkout path, when the workspace was created from one.
    pub fn worktree_checkout(&self) -> Option<PathBuf> {
        self.worktree
            .as_ref()
            .map(|worktree| PathBuf::from(&worktree.checkout_path))
    }
}

/// Pick the pane whose cwd best represents the workspace. Panes in one workspace can sit in
/// unrelated repos, so the focused pane wins, then any pane in the active tab, then the first.
///
/// A pane herdr reports no cwd for cannot represent the workspace, so it is passed over rather
/// than chosen and resolved to nothing. A plugin pane's cwd is always the plugin's own install
/// directory, never the workspace's repo, so it is passed over too.
pub fn representative_pane(panes: &[Pane], active_tab_id: &str) -> Option<PathBuf> {
    let located = || {
        panes
            .iter()
            .filter(|pane| pane.cwd.is_some() && pane.label.is_none())
    };
    located()
        .find(|pane| pane.focused)
        .or_else(|| located().find(|pane| pane.tab_id == active_tab_id))
        .or_else(|| located().next())
        .and_then(|pane| pane.cwd.as_deref())
        .map(PathBuf::from)
}

/// Group panes by the workspace that owns them, so one snapshot serves every workspace in a pass.
pub fn panes_by_workspace(panes: Vec<Pane>) -> HashMap<String, Vec<Pane>> {
    let mut grouped: HashMap<String, Vec<Pane>> = HashMap::new();
    for pane in panes {
        grouped
            .entry(pane.workspace_id.clone())
            .or_default()
            .push(pane);
    }
    grouped
}

/// Take one consistent view of every workspace and pane.
///
/// The reporter uses this instead of a workspace listing plus one pane listing per workspace. It
/// costs the same as the workspace listing alone, and a workspace closing mid-pass can no longer
/// tear the two apart.
pub fn snapshot(timeout: Duration) -> Result<Snapshot> {
    let response: SnapshotResponse =
        exec::run_json_timed(&context::herdr_bin(), &["api", "snapshot"], None, timeout)?;
    Ok(response.result.snapshot)
}

/// List the panes of one workspace.
pub fn pane_list(workspace_id: &str, timeout: Duration) -> Result<Vec<Pane>> {
    let response: PaneListResponse = exec::run_json_timed(
        &context::herdr_bin(),
        &["pane", "list", "--workspace", workspace_id],
        None,
        timeout,
    )?;
    Ok(response.result.panes)
}

/// Focus a plugin-owned pane, switching tabs if it lives in another one.
pub fn focus_pane(pane_id: &str) -> Result<()> {
    let args = ["plugin", "pane", "focus", pane_id];
    exec::run_ok(&context::herdr_bin(), &args, None)
}

/// Close a plugin-owned pane.
pub fn close_pane(pane_id: &str) -> Result<()> {
    let args = ["plugin", "pane", "close", pane_id];
    exec::run_ok(&context::herdr_bin(), &args, None)
}

/// Raise a notification in herdr's corner, where its own config diagnostics appear.
///
/// The reporter bounds this call to protect its poll loop. A successful command can still report
/// `disabled`, `rate_limited`, `no_foreground_client`, or `busy` without showing a toast.
pub fn notify(title: &str, body: &str, timeout: Duration) -> Result<()> {
    let args = [
        "notification",
        "show",
        title,
        "--body",
        body,
        "--position",
        "top-right",
        "--sound",
        "none",
    ];
    exec::run_ok_timed(&context::herdr_bin(), &args, None, timeout)
}

/// Rename a tab.
///
/// Opening a pane takes no title, so a tab is named after it exists rather than at creation.
pub fn rename_tab(tab_id: &str, label: &str) -> Result<()> {
    let args = ["tab", "rename", tab_id, label];
    exec::run_ok(&context::herdr_bin(), &args, None)
}

/// Set the workspace's `$roborev_*` tokens, clearing each one whose value is `None`.
///
/// Sets and clears mix in one request, keeping the counts in the same sidebar frame.
///
/// `seq` prevents a slow pass from overwriting a newer one. Every request must include it because
/// omitting it bypasses herdr's sequence check without advancing the stored value.
pub fn report_tokens(
    workspace_id: &str,
    values: [Option<String>; 3],
    seq: u64,
    ttl: Duration,
    timeout: Duration,
) -> Result<()> {
    let mut args = vec![
        "workspace".to_string(),
        "report-metadata".to_string(),
        workspace_id.to_string(),
        "--source".to_string(),
        SOURCE.to_string(),
    ];

    let mut any_set = false;
    for (name, value) in TOKENS.iter().zip(values) {
        match value {
            Some(value) => {
                any_set = true;
                args.push("--token".to_string());
                args.push(format!("{name}={value}"));
            }
            None => {
                args.push("--clear-token".to_string());
                args.push(name.to_string());
            }
        }
    }

    // A TTL applies to the tokens being set, so a request that only clears has nothing to expire.
    if any_set {
        args.push("--ttl-ms".to_string());
        args.push(ttl.as_millis().to_string());
    }

    args.push("--seq".to_string());
    args.push(seq.to_string());

    exec::run_ok_timed(&context::herdr_bin(), &args, None, timeout)
}

/// Open one of this plugin's registered pane entrypoints.
///
/// Popups target the active pane and accept no workspace or target. Splits divide `target_pane`,
/// and `env` entries configure the new pane process.
pub fn open_pane(
    entrypoint: &str,
    target_pane: Option<&str>,
    env: &[(&str, String)],
) -> Result<()> {
    open(entrypoint, target_pane, env, None)
}

/// Open a pane entrypoint as a tab, overriding the placement its manifest declares.
pub fn open_pane_in_tab(entrypoint: &str) -> Result<()> {
    open(entrypoint, None, &[], Some("tab"))
}

fn open(
    entrypoint: &str,
    target_pane: Option<&str>,
    env: &[(&str, String)],
    placement: Option<&str>,
) -> Result<()> {
    let mut args = vec![
        "plugin".to_string(),
        "pane".to_string(),
        "open".to_string(),
        "--plugin".to_string(),
        crate::PLUGIN_ID.to_string(),
        "--entrypoint".to_string(),
        entrypoint.to_string(),
    ];

    if let Some(placement) = placement {
        args.push("--placement".to_string());
        args.push(placement.to_string());
    }

    if let Some(pane) = target_pane {
        args.push("--target-pane".to_string());
        args.push(pane.to_string());
    }

    for (key, value) in env {
        args.push("--env".to_string());
        args.push(format!("{key}={value}"));
    }

    exec::run_ok(&context::herdr_bin(), &args, None)
}

#[cfg(test)]
mod tests {
    use super::{
        Pane, PaneListResponse, SnapshotResponse, panes_by_workspace, representative_pane,
    };

    #[test]
    fn parses_the_api_snapshot_envelope() {
        let response: SnapshotResponse = serde_json::from_str(
            r#"{"id":"cli:api:snapshot","result":{"type":"session_snapshot","snapshot":{
                "version":"0.8.0","protocol":16,"focused_workspace_id":"w1",
                "agents":[],"tabs":[],"layouts":[],
                "workspaces":[
                    {"workspace_id":"w1","number":1,"label":"api","focused":true,
                     "active_tab_id":"w1:t1","agent_status":"unknown",
                     "worktree":{"repo_key":"k","repo_name":"api","repo_root":"/repo",
                                 "checkout_path":"/repo/wt","is_linked_worktree":true}},
                    {"workspace_id":"w2","number":2,"label":"scratch","focused":false,
                     "active_tab_id":"w2:t1","agent_status":"unknown"}],
                "panes":[
                    {"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","focused":true,
                     "cwd":"/repo/wt","agent_status":"unknown","revision":3},
                    {"pane_id":"w2:p1","workspace_id":"w2","tab_id":"w2:t1","focused":false,
                     "cwd":"/scratch","agent_status":"unknown","revision":1}]}}}"#,
        )
        .expect("valid envelope");

        let snapshot = response.result.snapshot;
        assert_eq!(snapshot.workspaces.len(), 2);
        assert_eq!(
            snapshot.workspaces[0].worktree_checkout().unwrap().to_str(),
            Some("/repo/wt")
        );
        assert!(snapshot.workspaces[1].worktree_checkout().is_none());

        let grouped = panes_by_workspace(snapshot.panes);
        assert_eq!(grouped["w1"][0].pane_id, "w1:p1");
        assert_eq!(grouped["w2"][0].cwd.as_deref(), Some("/scratch"));
    }

    /// Herdr omits `cwd` rather than nulling it, so a pane without one must not fail the envelope.
    #[test]
    fn a_pane_without_a_cwd_still_parses() {
        let response: PaneListResponse = serde_json::from_str(
            r#"{"id":"cli:pane:list","result":{"type":"pane_list","panes":[
                {"pane_id":"w1:p4","workspace_id":"w1","tab_id":"w1:t4","focused":false,
                 "agent_status":"unknown","revision":10}]}}"#,
        )
        .expect("valid envelope");
        assert!(response.result.panes[0].cwd.is_none());
    }

    #[test]
    fn parses_the_pane_list_envelope() {
        let response: PaneListResponse = serde_json::from_str(
            r#"{"id":"cli:pane:list","result":{"type":"pane_list","panes":[
                {"pane_id":"w1:p4","workspace_id":"w1","tab_id":"w1:t4","focused":false,
                 "cwd":"/repo","foreground_cwd":"/repo","agent_status":"unknown","revision":10}]}}"#,
        )
        .expect("valid envelope");
        assert_eq!(response.result.panes[0].cwd.as_deref(), Some("/repo"));
        assert_eq!(response.result.panes[0].pane_id, "w1:p4");
        assert!(response.result.panes[0].label.is_none());
    }

    #[test]
    fn a_plugin_pane_carries_its_manifest_title_as_a_label() {
        let response: PaneListResponse = serde_json::from_str(
            r#"{"id":"cli:pane:list","result":{"type":"pane_list","panes":[
                {"pane_id":"w8:p1B","workspace_id":"w8","tab_id":"w8:t1","focused":false,
                 "cwd":"/repo","label":"roborev","agent_status":"unknown","revision":0}]}}"#,
        )
        .expect("valid envelope");
        assert_eq!(response.result.panes[0].label.as_deref(), Some("roborev"));
    }

    fn pane(cwd: &str, tab_id: &str, focused: bool) -> Pane {
        located_pane(Some(cwd), tab_id, focused)
    }

    fn located_pane(cwd: Option<&str>, tab_id: &str, focused: bool) -> Pane {
        Pane {
            cwd: cwd.map(str::to_string),
            workspace_id: "w1".to_string(),
            tab_id: tab_id.to_string(),
            focused,
            pane_id: format!("{tab_id}:{}", cwd.unwrap_or("none")),
            label: None,
        }
    }

    fn plugin_pane(cwd: &str, tab_id: &str, focused: bool) -> Pane {
        Pane {
            label: Some("roborev".to_string()),
            ..pane(cwd, tab_id, focused)
        }
    }

    #[test]
    fn focused_pane_wins() {
        let panes = [
            pane("/a", "t1", false),
            pane("/b", "t2", true),
            pane("/c", "t1", false),
        ];
        assert_eq!(
            representative_pane(&panes, "t1").unwrap().to_str(),
            Some("/b")
        );
    }

    #[test]
    fn active_tab_wins_when_nothing_is_focused() {
        let panes = [pane("/a", "t1", false), pane("/b", "t2", false)];
        assert_eq!(
            representative_pane(&panes, "t2").unwrap().to_str(),
            Some("/b")
        );
    }

    #[test]
    fn falls_back_to_the_first_pane() {
        let panes = [pane("/a", "t1", false)];
        assert_eq!(
            representative_pane(&panes, "t9").unwrap().to_str(),
            Some("/a")
        );
        assert!(representative_pane(&[], "t1").is_none());
    }

    #[test]
    fn a_pane_without_a_cwd_is_passed_over() {
        // The focused pane would win on order alone, so this proves the filter runs first.
        let panes = [
            located_pane(None, "t1", true),
            located_pane(Some("/b"), "t2", false),
        ];
        assert_eq!(
            representative_pane(&panes, "t9").unwrap().to_str(),
            Some("/b")
        );
        assert!(representative_pane(&[located_pane(None, "t1", true)], "t1").is_none());
    }

    #[test]
    fn a_focused_plugin_pane_is_passed_over() {
        // A plugin pane's cwd is the plugin's own install directory, never the workspace's repo.
        let panes = [plugin_pane("/plugin", "t1", true), pane("/b", "t2", false)];
        assert_eq!(
            representative_pane(&panes, "t2").unwrap().to_str(),
            Some("/b")
        );
        assert!(representative_pane(&[plugin_pane("/plugin", "t1", true)], "t1").is_none());
    }
}
