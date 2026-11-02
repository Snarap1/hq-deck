# hq-deck dev tasks - run `just <task>` (https://github.com/casey/just)

# default: list tasks
default:
    @just --list

# format the code
fmt:
    cargo fmt --all

# check formatting
fmt-check:
    cargo fmt --all --check

# lint with clippy, warnings as errors
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# run the test suite
test:
    cargo test --all-features

# build (debug)
build:
    cargo build

# run deck in the current repo
run:
    cargo run

# build release and install `hq-deck` into ~/.cargo/bin (atomic replace, safe while running)
install:
    cargo install --path . --locked

# PTY smoke test of the editor path against a real release binary
smoke-edit:
    cargo build --release
    python3 scripts/smoke_edit_file.py --binary target/release/hq-deck

# the full check: fmt-check, lint, test, release build
ci: fmt-check lint test
    cargo build --release

# install the herdr-app mod and its key (super-y opens hq-deck in a pane to the right)
install-herdr:
    scripts/install_herdr.sh
