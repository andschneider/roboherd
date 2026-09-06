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
# environment is what scopes both to that session.
stop-reporter:
    {{justfile_directory()}}/bin/roboherd stop-reporter

# Start this session's reporter and wait for readiness.
start-reporter:
    {{justfile_directory()}}/bin/roboherd start-reporter
