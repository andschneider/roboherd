# roboherd

<p align="left">
  <img src="assets/logo.svg" alt="roboherd" width="300" />
</p>

A [herdr](https://github.com/herdrdev/herdr) plugin that integrates
[roborev](https://github.com/kenn-io/roborev) into your workspace.

---

See review status in your workspace sidebar and get notified when a review starts or finishes. Start
a review with one or more agents from a popup, use roborev's TUI without leaving your tab, or open a
quick view split with the latest review and quick actions.

Browsing the queue, rerunning jobs, and broader review management stay in `roborev tui`, which this
plugin launches rather than reimplements.

## Requirements

- **herdr ≥ 0.7.5**
- **roborev ≥ 0.63**
- **macOS or Linux**

## Quick start

```bash
herdr plugin install andschneider/roboherd
```

Bind each entrypoint to a key in your herdr config:

```toml
# Open quick view split.
[[keys.command]]
key = "ctrl+shift+o"
# Direct chord, no prefix.
type = "plugin_action"
command = "roboherd.open-review"
description = "open review"

# Open the commit picker to start a review.
[[keys.command]]
# Direct chord, no prefix.
key = "ctrl+shift+p"
type = "plugin_action"
command = "roboherd.review-commit"
description = "review commit"

# Open roborev tui in a pop up or tab.
[[keys.command]]
key = "prefix+quote"
type = "plugin_action"
command = "roboherd.open-tui"
description = "open roborev tui"
```

Or invoke one directly, without a bind:

```bash
herdr plugin action invoke open-tui --plugin roboherd
```

> Reviews shown in the sidebar status, the quick view pane, and the full roborev TUI are all scoped
> to your current **repo** and **branch**.

### Workspace bar

To see review status in your workspace bar, you need to add the icons. Add the three `$roborev_*`
tokens to a sidebar space row in your herdr config:

```toml
[ui.sidebar.spaces]
rows = [
    ["state_icon", "workspace"],
    [
        "branch",
        { token = "$roborev_f", fg = "#bf616a" },
        { token = "$roborev_p", fg = "#a3be8c" },
        "$roborev_r",
    ],
]
```

The row renders as `×2 · ✓1 · ●1`: open failing reviews, open passing ones, and queued or running
ones. A count at zero drops out, so the row disappears when nothing is outstanding.

## Starting a review

`review-commit` opens a popup listing the most recent commits. Select one or more commits and then
change the reviewer agent or review type.

| Key                                           | Does                                             |
|-----------------------------------------------|--------------------------------------------------|
| arrows, `j`/`k`, PgUp/PgDn, Home/`g`, End/`G` | Move                                             |
| `v`, space                                    | Mark one end of a range, the cursor is the other |
| `t`                                           | Cycle the review type                            |
| `a`                                           | Choose agents                                    |
| enter                                         | Start                                            |
| escape                                        | Clear a marked range, else close the popup       |
| `q`, control-C                                | Close the popup                                  |

When the working tree is dirty, a `dirty` row sits above the log with a count of changed files.
Selecting it reviews the uncommitted work, including staged, unstaged, and untracked files.

### Choosing agents and review type

#### Agents

Press `a` to open a list of the installed agents, with the roborev configured default checked.
Checking several starts the same commits once per agent, giving one independent job each.

| Key             | Does                              |
|-----------------|-----------------------------------|
| arrows, `j`/`k` | Move                              |
| space           | Toggle                            |
| enter, `a`, `q` | Confirm and return to the commits |
| escape          | Discard changes and return        |
| control-C       | Close the popup                   |

Leaving the default untouched keeps roborev's configured choice, which preserves its reasoning-tier
agent overrides. An empty selection hands the choice back to roborev the same way.

#### Review type

`t` cycles the review type, in this order:

- `default` - general review of the commit. Looks for bugs, regressions, gaps between what the
  commit message claims and what the diff does, missing tests, and code quality problems.
- `security` - vulnerabilities only, held to an exploitability burden of proof. Covers injection,
  auth and access control, credential exposure, path traversal, unsafe deserialization, and CI
  workflow injection. Generic hardening advice is suppressed, so a clean run is common.
- `design` - for commits carrying design docs such as PRDs, task lists, and architecture proposals.
  Flags internal contradictions first, then completeness, feasibility against the actual codebase,
  and whether the task stages are ordered and small enough to review.

## Reading a review

`open-review` renders the markdown from `roborev show` in a wrapping pane, picking the newest review
worth reading: a completed, unclosed, failing one outranks a newer passing one.

| Key                                  | Does                       |
|--------------------------------------|----------------------------|
| `j`/`k`                              | Scroll older/newer reviews |
| arrows, PgUp/PgDn, Home/`g`, End/`G` | Scroll review text         |
| `r`                                  | Refresh review             |
| `a`                                  | Close review               |
| `c`                                  | Comment on review          |
| `q`, escape, control-C               | Close the pane             |

`c` opens a small editor for the comment:

| Key       | Does             |
|-----------|------------------|
| enter     | Submit           |
| control-J | Insert a newline |
| escape    | Cancel           |

## Entrypoints

| Entrypoint      | Placement                      | Toggles  | Shows                                          |
|-----------------|--------------------------------|----------|------------------------------------------------|
| `open-review`   | split                          | yes      | Newest review with basic commands.             |
| `open-tui`      | [popup or tab](#configuration) | as a tab | Full `roborev tui`, scoped to repo and branch. |
| `review-commit` | popup                          | no       | The commit picker.                             |

Key binds for each entrypoint are under [Quick start](#quick-start).

## Configuration

roboherd has its own optional config file, separate from herdr's:

```text
~/.config/herdr/plugins/config/roboherd/config.toml
```

```toml
# popup or tab
tui_placement = "popup"
```

Available options:

| Key             | Default | Values         | Effect                          |
|-----------------|---------|----------------|---------------------------------|
| `tui_placement` | `popup` | `popup`, `tab` | Where `open-tui` opens roborev. |

## Development

Use the `justfile` to develop and test the plugin.

```bash
just          # lint and test
just lint     # fmt --check and clippy with warnings denied
just test     # cargo test
just package  # build the plugin
just link     # link this checkout as a herdr plugin
just unlink   # remove the linked plugin
```

If you change the reporter, run `just restart-reporter` to restart it.

## Behavior notes

Some behavior can look surprising without the constraints behind it. See
[docs/design.md](docs/design.md) for the cross-module design.

### Notifications

The reporter notifies on reviews that start or finish. The first poll after a restart is a silent
baseline, so restarting roboherd never replays history, and a review that starts and finishes within
one polling interval only produces a finished notification.

Several transitions in one pass collapse into a single toast. Notifications are best-effort, so
disabled delivery, another visible toast, rate limiting, or no foreground client can each keep one
from appearing. The sidebar token is the durable signal, and it updates independently.

### Ranges and ancestry

A marked range becomes `roborev review START END`, which reviews `START^..END` inclusive. Because
`git log` lists newest first, the older end of the range sits further down the list.

Before enqueuing, the picker checks the range against git and refuses one that would add or skip
commits beyond the highlighted rows, naming how many. The same applies to a range reaching the root
commit, since there is no `START^`.

### Agents and the daemon

Only agents whose command is on `PATH` are offered, so a name roborev accepts can still be missing
from the list. The one labelled default comes from `roborev config get`, which merges repo config
over global, and roborev layers reasoning-tier overrides above that, so the label is indicative
rather than a guarantee.

`roborev list` can start or restart the roborev daemon, so this plugin keeps one alive whenever it's
enabled.

### Panes and popups

`tui_placement` fixes `open-tui`'s placement at open time, since a popup has no pane id to toggle or
move later.

A pane instead focuses an existing instance when it's elsewhere, and closes it when it's already
focused. Popups skip this, since they're transient and never toggle.
