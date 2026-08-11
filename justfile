# roboherd task runner

# Where a hand-restarted reporter writes, since only herdr's own startup captures its output.
reporter-log := "/tmp/roboherd-reporter.log"

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

# Herdr runs [[startup]] only at server start, so a killed reporter is never respawned.
# Replace the running reporter with the staged binary, logging to reporter-log
restart-reporter:
    # The bracket keeps each pattern from matching the shell running this recipe, whose own argv
    # holds the pattern. Without it pkill signals that shell and the wait loop never ends.
    -pkill -f '{{justfile_directory()}}/bin/[r]oboherd reporter'
    # The lock releases on exit, so the replacement waits rather than racing it.
    while pgrep -f '{{justfile_directory()}}/bin/[r]oboherd reporter' >/dev/null; do sleep 0.1; done
    nohup {{justfile_directory()}}/bin/roboherd reporter >>{{reporter-log}} 2>&1 &
    sleep 0.3
    @pgrep -f '{{justfile_directory()}}/bin/[r]oboherd reporter' >/dev/null \
        && echo "reporter restarted, logging to {{reporter-log}}" \
        || { echo "reporter failed to start, see {{reporter-log}}"; exit 1; }
