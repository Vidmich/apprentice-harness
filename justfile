# apprentice-harness task runner. Run `just` to list recipes.

set windows-shell := ["pwsh", "-NoProfile", "-Command"]

default:
    @just --list

# Build every Rust crate
build:
    cargo build --workspace

# Run all tests: Rust, Python, GUI
test: test-rust test-ml test-gui

test-rust:
    cargo test --workspace

test-ml:
    uv run --directory ml pytest -q

test-gui:
    pnpm --dir apps/gui test

# Lint everything (fails on warnings)
lint: lint-rust lint-ml lint-gui

lint-rust:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

lint-ml:
    uv run --directory ml ruff check .
    uv run --directory ml ruff format --check .

lint-gui:
    pnpm --dir apps/gui typecheck
    pnpm --dir apps/gui lint

# Format all sources in place
fmt:
    cargo fmt --all
    uv run --directory ml ruff format .
    pnpm --dir apps/gui format

# Build + lint + test, the same set CI runs
check: build lint test

# Run the GUI in development mode (set HARNESS_HOME for an isolated data dir)
gui:
    pnpm --dir apps/gui tauri dev

# Build the release daemon and copy it into place as the GUI sidecar
gui-sidecar:
    pnpm --dir apps/gui sidecar

# Build the installer (bundles the daemon as a sidecar)
gui-build:
    pnpm --dir apps/gui build:app

# Run the daemon in the foreground
daemon *ARGS:
    cargo run -p harnessd -- {{ARGS}}

# Run the CLI
cli *ARGS:
    cargo run -p harness -- {{ARGS}}

# Install frontend and Python dependencies
setup:
    pnpm --dir apps/gui install
    uv sync --directory ml
