# Design

The parts of roboherd that no single file explains. Rules are in [CLAUDE.md](../CLAUDE.md), details
are in doc comments next to the code. This covers only what spans modules.

Keep it short. If something fits in a doc comment beside the code it describes, it belongs there
instead, where it is far likelier to be updated with the code.

## Shape

One reporter process, launched from `[[startup]]`, polls roborev for every open workspace and
publishes its sidebar tokens with a TTL a few intervals long. Panes and popups are separate
short-lived processes that herdr spawns per invocation.

Nothing shares process-local state between them. The pane listing and roborev's own job list are the
source of truth, so there is no internal cache to invalidate. If the reporter dies, its last
published metadata remains until the TTL expires. Everything reaching roborev or git goes through
the CLI, run from the resolved checkout.

## External constraints

Properties of herdr, git, roborev, and macOS. They explain choices that look arbitrary otherwise,
and they hold no matter how roboherd is structured.

- **herdr shows one toast at a time.** A pass carrying several transitions folds into one summary
  notification, because raising one per transition would drop all but the first.
- **A herdr workspace has no cwd unless it was made by `worktree create`.** The checkout is resolved
  from a pane instead, in a fixed order. Panes in one workspace can sit in unrelated repos, so an
  arbitrary pick would report a different repo from one poll to the next.
- **A popup has no pane id.** It can be neither toggled nor moved into the layout, and cannot be
  promoted to a tab once open, so placement is chosen at open time. One TUI entrypoint covers both,
  because `plugin pane open --placement` overrides what the manifest declares.
- **`git log` orders by date, not ancestry.** Rows adjacent on screen need not be adjacent in
  ancestry, so a marked range is checked against git before it is enqueued.
- **roborev's `check-agents` runs each agent for real.** Far too slow for a keypress, so agent
  availability is a `PATH` check against a mirrored name-to-command table.
- **macOS `SIGKILL`s a Mach-O overwritten in place.** The staged binary is removed before copying,
  or every later exec of the reporter dies with no exit code and no stderr.
- **`tui-markdown` sizes a table to its widest cell and takes no width limit.** A table wider than
  the review pane arrives as box-drawing that wrapping shreds, accepted because normal reviews
  rarely contain tables.

## Deliberately absent

No SSE or event subscription. One would need reconnect logic, backoff, and a resync path for events
missed while disconnected, all to save a subprocess every few seconds. The TTL handles a dead
reporter instead, with no liveness protocol to write.

No per-workspace watcher, because one process holding one lock and iterating needs no lifecycle
management as workspaces open and close.
