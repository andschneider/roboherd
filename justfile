# roboherd task runner

# Run fmt, clippy, and tests
default: lint test

# Format all sources
fmt:
    cargo fmt --all

# Check formatting and lint with warnings denied
lint:
    cargo fmt --all --check
    cargo clippy --all-targets --locked -- --deny warnings

# Type-check without producing a binary
check:
    cargo check --all-targets --locked

# Run the test suite
test:
    cargo test --locked

# Install the binary into the cargo bin directory
install:
    cargo install --path .

# Link this checkout as a herdr plugin for local development
link:
    herdr plugin link {{justfile_directory()}}

# Remove the linked development plugin
unlink:
    herdr plugin unlink roboherd

# Stage a release binary at the manifest's bin/roboherd path
# The reporter runs from bin/roboherd, and overwriting a mapped Mach-O in place makes macOS
# SIGKILL every later exec. Unlink first so the copy lands on a fresh inode.
package:
    cargo build --release --locked
    mkdir -p bin
    rm -f bin/roboherd
    cp target/release/roboherd bin/roboherd

# Herdr runs [[startup]] only at server start, so a killed reporter is never respawned, and each
# session locks its own reporter beside its own socket. Run stop-reporter then start-reporter from
# inside the session whose reporter you want to bounce -- HERDR_SOCKET_PATH in that pane's own
# environment is what scopes both to that session, since pgrep can't tell sessions apart (every
# reporter shares the same argv).
stop-reporter:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${HERDR_SOCKET_PATH:?run this inside a herdr session}"
    lock="$(dirname "$HERDR_SOCKET_PATH")/roboherd-reporter.lock"
    pid=$(lsof -t "$lock" 2>/dev/null || true)
    [ -n "$pid" ] || { echo "no reporter running for this session"; exit 0; }
    # The stream child goes first, by parent pid, so a hand-run `roborev stream` is untouched --
    # it would otherwise outlive the reporter until roborev next broadcasts, leaking one per kill.
    pkill -P "$pid" 2>/dev/null || true
    kill "$pid"
    while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done
    echo "reporter stopped"

# Start this session's reporter in the background, logging beside its lock. Run inside the session
# you want it to report for.
start-reporter:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${HERDR_SOCKET_PATH:?run this inside a herdr session}"
    log="$(dirname "$HERDR_SOCKET_PATH")/roboherd-reporter.log"
    nohup {{justfile_directory()}}/bin/roboherd reporter >>"$log" 2>&1 &
    sleep 0.3
    lock="$(dirname "$HERDR_SOCKET_PATH")/roboherd-reporter.lock"
    if [ -f "$lock" ] && lsof "$lock" >/dev/null 2>&1; then
        echo "reporter started, logging to $log"
    else
        echo "reporter failed to start, see $log"
        exit 1
    fi
