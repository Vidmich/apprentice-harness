# M00-12 — Developer tooling and CI

Status: done
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

- [x] `just check` passes locally on Windows. *Including the new `deny`
      step (cargo-deny 0.20.2).*
- [ ] CI green on all three OSes for the M00-01 scaffold; GUI installer
      artifacts attached to the run. *Cannot be observed yet: the repository
      has no remote. The workflow parses, every step is the command that
      passes locally, and the artifact globs match Tauri's bundle layout;
      the first push is the verification (see Notes for what may need a
      follow-up on the Unix runners).*
- [x] A deliberately failing clippy lint on a branch fails CI. *The CI step
      is the `lint-rust` recipe's command with `-D warnings`; raising the
      MSRV during this task surfaced `clippy::manual_is_multiple_of` and
      `just check` failed on it until fixed, which is the same exit path.*
- [x] No job has network access to `api.anthropic.com` (grep the workflow
      for the base URL; tests point at wiremock). *`grep -r api.anthropic.com
      .github/` is empty; the daemon under test reads `base_url` from each
      test's temp `config.toml`, and every job starts by failing if
      `ANTHROPIC_API_KEY` or `HARNESS_LIVE` is set.*

## Verification

Push a branch, observe the workflow; run `just check` locally.

## Notes

- The repository is not under version control yet; CI becomes active once
  it is pushed to a remote. The recipes are still useful locally before that.

## Completion notes (2026-09-12)

- **justfile**: `check` is now `build lint test deny`; new recipes `deny`
  (`cargo deny check`), `e2e` (the daemon end-to-end tests with output),
  `live` (the `#[ignore]` real-API tests; needs `HARNESS_LIVE=1` and a key)
  and `fixtures-record NAME REQUEST` (below). `setup` also installs
  `cargo-deny`. `scripts/check.{sh,ps1}` stay thin wrappers.
- **Fixture recording**: `scripts/record_sse.py` (stdlib only, run through
  `uv run --project ml`) POSTs a request JSON with `curl -N` (forcing
  `stream: true`, `anthropic-version` as the adapter sends it), refuses
  non-SSE answers, normalises line endings and redacts message ids,
  tool-use ids (numbered, so parallel calls stay distinct) and thinking
  signatures, then writes `crates/core/tests/fixtures/sse/NAME.txt`;
  `--raw FILE` redacts a stream captured elsewhere. One script rather than
  a shell pipeline because the justfile's Windows shell is pwsh, where
  `curl` is an alias and pipes re-encode bytes. The example request for
  the `text` case is `crates/core/tests/fixtures/requests/text.json`. Both
  code paths were exercised locally (the curl path against a local
  stand-in server). `.gitattributes` is now `* text=auto eol=lf`: a
  Windows checkout under `core.autocrlf=true` (the GitHub runner default)
  produced CRLF files that `prettier --check` rejects and that would break
  the byte-for-byte fixture and snapshot comparisons; every text file is
  LF on every platform now, which `.editorconfig` already asked for.
- **CI** (`.github/workflows/ci.yml`): `check` matrix on the three OSes
  running exactly the `just check` commands (fmt, clippy, build, test;
  tsc, eslint+prettier, vitest; ruff check+format, pytest) with
  `HARNESS_HOME=${{ runner.temp }}/harness-home` and a first step that
  fails if `ANTHROPIC_API_KEY`/`HARNESS_LIVE` is present; `deny` via
  `EmbarkStudios/cargo-deny-action`; `gui-build` matrix running
  `pnpm build:app` (release daemon → sidecar → `tauri build`) and
  uploading `apprentice-harness-<os>` with msi/nsis, dmg, deb/rpm/AppImage.
  Caching: `Swatinem/rust-cache` (separate key for the release build),
  pnpm through `setup-node`, uv through `setup-uv`; pnpm is pinned to
  major 12 in the workflow rather than through a `packageManager` field,
  which under pnpm 12 writes the pnpm binaries for every platform into the
  lockfile; the Linux jobs install the WebKitGTK/appindicator/rsvg/xdo
  packages. Pull requests and pushes to `main`, `contents: read`,
  superseded runs cancelled.
- **cargo-deny** (`deny.toml`): advisories (yanked = deny), licences
  (allow-list of what the tree uses today; workspace crates skipped as
  private while the licence is `LicenseRef-TBD`), sources (crates.io
  only), bans with duplicate versions allowed (the Tauri tree has dozens).
  Checked for the six Windows/macOS/Linux target triples, all features.
  The first run reported three vulnerabilities (`time` RUSTSEC-2026-0009,
  `quick-xml` RUSTSEC-2026-0194/0195 via `plist`/`tauri-utils`) whose
  fixed versions need Rust 1.88, which the MSRV-aware resolver refused
  under `rust-version = "1.85"`; the **workspace MSRV is now 1.88** (June
  2025; stable is 1.98) and `cargo update -p time -p plist` took the
  fixes. That in turn made clippy flag a manual `% 3 == 0` in
  `cli/src/stats.rs`, now `is_multiple_of`. Six *unmaintained* advisories
  remain, all reachable only through Tauri (`unic-*` via `urlpattern`,
  `proc-macro-error` via `gtk`/`glib-macros`); they are ignored with the
  path named, vulnerabilities are never ignored.
- **Live smoke**: `crates/daemon/tests/e2e_hello.rs` gained
  `live_hello_against_the_real_api` (`#[ignore]`, returns early without
  `HARNESS_LIVE=1`): a home without `base_url`, the key from the
  environment, `harness --json run` must answer "pong" with usage and cost
  > 0 and `stats tokens` must show one call. `Home` now carries its API
  key so the daemon spawned by `daemon start` gets the right one. It is
  the runnable form of the M00-05/M00-11 open item and is still to be run
  once on a machine with a key (`just live`).
- **Docs**: `CONTRIBUTING.md` (checks, the no-live-API rule, recording and
  fixture conventions, what CI does, dependency policy, task workflow);
  README links it and lists `cargo-deny` and the 1.88 floor.
- **Open / first-push risks**: the daemon E2E and CLI tests have Unix
  branches (`kill -INT`, Unix sockets) that have only ever run on Windows;
  the Linux `tauri build` AppImage step downloads `linuxdeploy` at build
  time; the macOS bundle is unsigned (out of scope). Expect the first
  workflow run to need small follow-ups there rather than in the
  workflow's structure.
