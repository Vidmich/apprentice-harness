# M00-12 — Developer tooling and CI

Status: todo
Depends on: M00-01
Size: S

## Goal

One-command local checks and a CI pipeline that builds and tests the Rust
workspace on Windows, macOS and Linux, lints Rust/TypeScript/Python, builds
the Tauri app, and never calls the real Anthropic API.

## Context

The project targets three platforms (SPEC §16); regressions on the platforms
the developer is not using must be caught automatically. Tests use mock
servers only (`tasks/README.md`).

## Scope

In: `just` recipes, pre-commit style local check script, GitHub Actions
workflow (matrix), caching, artifact upload of the GUI installer, test
fixture conventions.
Out: release signing/notarisation, publishing.

## Design

- `justfile` (from M00-01) gains: `check` (fmt --check, clippy -D warnings,
  test, ruff, eslint, tsc --noEmit), `fixtures-record` (documented manual
  recipe using `curl -N` to capture SSE fixtures with redaction), `e2e`.
- `.github/workflows/ci.yml`:
  - matrix `os: [windows-latest, macos-latest, ubuntu-latest]`;
  - Rust: `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache`;
    `cargo build --workspace`, `cargo test --workspace`, `cargo clippy
    --all-targets -- -D warnings`, `cargo fmt --check`;
  - Node: pnpm + cache; `pnpm --dir apps/gui install --frozen-lockfile`,
    `pnpm lint`, `pnpm test`, `pnpm tauri build` (Linux job installs
    webkit2gtk deps); upload installer artifacts;
  - Python: `astral-sh/setup-uv`; `uv run --directory ml pytest`,
    `uv run ruff check`.
  - Env: `HARNESS_HOME` set to a runner temp dir; `ANTHROPIC_API_KEY` unset;
    tests marked `#[ignore]`/`HARNESS_LIVE` never run in CI.
- `deny.toml` (`cargo-deny`) for licence/advisory checks — allow-list to be
  finalised when the project licence is chosen; advisories enforced now.
- `CONTRIBUTING.md`: how to run checks, fixture recording, task-file
  workflow (`tasks/README.md`).

## Acceptance

- [ ] `just check` passes locally on Windows.
- [ ] CI green on all three OSes for the M00-01 scaffold; GUI installer
      artifacts attached to the run.
- [ ] A deliberately failing clippy lint on a branch fails CI.
- [ ] No job has network access to `api.anthropic.com` (grep the workflow
      for the base URL; tests point at wiremock).

## Verification

Push a branch, observe the workflow; run `just check` locally.

## Notes

- The repository is not under version control yet; CI becomes active once
  it is pushed to a remote. The recipes are still useful locally before that.
