# Design

The parts of roboherd that no single file explains. Rules are in [CLAUDE.md](../CLAUDE.md), details
are in doc comments next to the code. This covers only what spans modules.

Keep it short. If something fits in a doc comment beside the code it describes, it belongs there
instead, where it is far likelier to be updated with the code.

## Shape

One reporter process per herdr session polls roborev for every open workspace and publishes its
sidebar tokens with a TTL a few intervals long. The `[[startup]]` hook runs `start-reporter`, which
detaches the long-running process after readiness. A `roborev stream` child touches the wake marker
on every line, cutting a finished review's wait from the poll interval to about a second. Panes and
popups are separate short-lived processes that herdr spawns per invocation.

Nothing shares process-local state between them. The pane listing and roborev's own job list are the
source of truth, so there is no internal cache to invalidate. Events carry no state, so a dropped
line costs latency and nothing else. If the reporter dies, its last published metadata remains until
the TTL expires. Everything reaching roborev or git goes through the CLI, run from the resolved
checkout.

## Reporter lifecycle

A session-scoped file lock enforces one reporter. Its private Unix socket handles readiness and
shutdown without trusting stored PIDs. The reporter atomically writes its process, pass, and stream
state beside the lock so diagnostics never wait for the polling loop.

Startup succeeds after a readiness reply. Until then, a private pipe ties the reporter to its
spawning CLI: closing it without confirmation triggers graceful cleanup. Shutdown acknowledges only
after releasing the stream child, socket, and lock.

Control requests are drained before each reconciliation. Stream reconnects also run on this loop,
so a slow pass delays both. See [the lifecycle commands](../src/commands/reporter.rs) and
[reporter loop](../src/reporter/poller.rs) for the implementation.

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
- **roborev's event stream is lossy and silent.** Each subscriber gets a ten event buffer that drops
  on overflow, and nothing is written between events, so a burst can go unseen and a wedged stream
  looks idle. Events shorten the wait for a pass and never replace one.
- **An orphaned `roborev stream` dies on its next write, not with its parent.** Go terminates on
  SIGPIPE at fd 1, so a killed reporter leaves the child holding a subscriber slot until roborev
  next broadcasts.
- **macOS `SIGKILL`s a Mach-O overwritten in place.** The staged binary is removed before copying,
  or every later exec of the reporter dies with no exit code and no stderr.
- **`tui-markdown` sizes a table to its widest cell and takes no width limit.** A table wider than
  the review pane arrives as box-drawing that wrapping shreds, accepted because normal reviews
  rarely contain tables.

## Deliberately absent

No SSE client or daemon subscription. `roborev stream` is a CLI subcommand run as a child, so
reconnecting is a spawn with a capped backoff checked on the reporter's existing tick. The local
control socket manages roboherd itself and never connects directly to the roborev daemon.

No per-workspace watcher, because one process holding one lock and iterating needs no lifecycle
management as workspaces open and close.
