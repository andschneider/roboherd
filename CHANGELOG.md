# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-09-07

### Added

- `start-reporter` and `stop-reporter` CLI commands for managing the reporter in a herdr session,
  with socket-based readiness and shutdown acknowledgments.
- An atomic reporter status file with process, reconciliation, and stream health details.
- A `doctor` command that reports the current session's reporter health.
- Startup warnings and doctor checks for the supported herdr and roborev versions.

### Changed

- The reporter owns its `roborev stream` child and kills and reaps it during socket shutdown.
- The manifest starts the reporter through the detached, readiness-checked launcher.
- `just start-reporter` and `just stop-reporter` delegate to the packaged Rust CLI.

### Fixed

- Scope reporter locks and control sockets to each herdr session so sessions can run independently.
- Drain queued control requests before reconciliation and clean up cancelled reporter startups
  without leaving their stream children behind.

## [0.2.0] - 2026-08-24

### Added

- Copy the current review to the clipboard with `y` in the quick view pane.
- Cycle the reasoning tier from the commit picker with `r`, requesting one of roborev's tiers by
  name when starting a review.

### Changed

- The reporter now wakes on `roborev stream` output in addition to its poll loop, lowering update
  latency without changing poll as the source of truth.

### Fixed

- Fixed review status and the commit picker's range handling inside git worktrees.

## [0.1.0] - 2026-08-10

### Added

- Initial release of `roboherd`!
