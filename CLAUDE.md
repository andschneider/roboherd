# CLAUDE.md

`roboherd` is one Rust binary backing a herdr plugin that surfaces roborev review state. See
[README.md](README.md) for what it does and [docs/design.md](docs/design.md) for why it is shaped
this way.

## Builds

Do not run `cargo build` or `cargo build --release`. Use `just check` to verify compilation and
`cargo run -- <subcommand>` to exercise the CLI. `just package` is the release path and belongs to
the user.

This project uses a `justfile`, not a Makefile.

## Boundaries

Decisions, not preferences. Changing one is a design change, so raise it rather than doing it.
[docs/design.md](docs/design.md) has the cross-module reasoning; single-concern reasoning lives in
doc comments beside the code it constrains.

- **The roborev CLI is the compatibility boundary.** No HTTP client, no generated OpenAPI client, no
  vendored daemon models, no direct daemon endpoints.
- **Explicit argv, never a shell.** External tools go through `src/exec.rs`. The reporter start
  command spawns its own executable directly to configure detachment and log redirection.
  Selected text and paths must never reach a command interpreter.
- **Commits come from git**, never from a regular expression over user text. A revision taken from
  anywhere but `git log` is validated with `git cat-file commit <rev>` before it reaches roborev,
  since `rev-parse --verify` accepts a SHA without reading its object and would miss one that's
  gone.
- **A range is git's answer, not the picker's.** Do not infer ancestry from row order, and do not
  "fix" the range check by listing with `--first-parent`, which would hide every commit merged in
  from a branch.
- **Poll is the source of truth; `roborev stream` is only a wake hint.** The reporter's poll loop
  is what publishes state, so a dropped or delayed stream line costs latency and nothing else. No
  SSE client or direct daemon subscription. Stream reconnects use a capped backoff checked on the
  reporter's own tick.
- **One reporter per herdr session for all its workspaces**, launched from `[[startup]]` or the
  explicit start command. Not one watcher per workspace, and not from event hooks.
- **Reporter control uses its own Unix socket.** Keep the file lock for singleton ownership.
  Only its holder may replace a stale socket. Readiness requires a reply, and shutdown releases
  the stream child, socket, and lock before acknowledging. Never signal a PID read from a file.
- **Workspace metadata only.** Never call `herdr pane report-agent` or otherwise participate in
  herdr's agent aggregation.
- **Review actions stay narrow.** The review pane may step between reviews, close its displayed job,
  or add a comment. Canceling, rerunning, and fixing belong to `roborev tui`.
- **Pane titles are unique.** A toggle finds its pane by matching the manifest `title` in a pane
  listing, so renaming a `[[panes]]` title is a behavior change rather than a cosmetic one.

Every binary subcommand named in `herdr-plugin.toml` maps to a `Command` variant and a function
under `src/commands/`. Keep those three in sync. One command module may back several related
subcommands, as `open_tui.rs` does.
