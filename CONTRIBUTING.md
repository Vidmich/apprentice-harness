# Contributing

How the repository is checked, how test fixtures are made, and how work is
organised. Design questions live in [SPEC.md](SPEC.md); the milestone plan in
[ROADMAP.md](ROADMAP.md); the decisions every task assumes in
[tasks/README.md](tasks/README.md).

## Prerequisites

See [README.md](README.md). `just setup` installs the frontend and Python
dependencies and `cargo-deny`; everything else is the stable Rust toolchain
(1.88 or newer), Node 22 with pnpm, and `uv`.

## Checks

```
just check        # build + lint + test + deny: what CI runs
just lint         # fmt --check, clippy -D warnings, ruff, eslint, prettier, tsc
just test         # cargo test --workspace, pytest, vitest
just deny         # cargo deny check (deny.toml)
just e2e          # the daemon end-to-end tests only, with output
just fmt          # format everything in place
```

`scripts/check.sh` and `scripts/check.ps1` are `just check` for hooks and
editors. Clippy runs with the pedantic group at workspace level and warnings
are errors, so `just lint` must be clean before a change is done.

The Rust integration tests spawn the real `harnessd` and `harness` binaries;
each test uses its own temporary `HARNESS_HOME`, so they never touch the
developer's configuration, traces or secrets.

## The mentor is never called from tests

Tests talk to a `wiremock` server that replays recorded SSE streams from
`crates/core/tests/fixtures/sse/`. The only code that reaches the real API
is behind two gates: the test is `#[ignore]`, and it returns early unless
`HARNESS_LIVE=1`. Run those deliberately, with a key, when a change touches
the wire protocol:

```
HARNESS_LIVE=1 ANTHROPIC_API_KEY=sk-ant-... just live
```

CI sets neither variable and refuses to start a job if `ANTHROPIC_API_KEY`
is present in its environment.

## Recording SSE fixtures

Fixtures are the raw `text/event-stream` body of one Messages API call,
one file per case, small. To capture a new one, write the request body as
JSON (see `crates/core/tests/fixtures/requests/text.json`; `stream: true`
is added for you) and run

```
ANTHROPIC_API_KEY=sk-ant-... just fixtures-record NAME path/to/request.json
```

which POSTs it with `curl -N` (`scripts/record_sse.py`) and writes
`crates/core/tests/fixtures/sse/NAME.txt`. The script normalises line
endings to LF and redacts what must not be committed or would make the
fixture brittle: message ids become `msg_01REDACTED`, tool-use ids
`toolu_01REDACTED` (numbered when a stream has several), thinking
signatures `REDACTED`. Usage counts and the model name are kept; tests
assert on them. A stream captured some other way goes through the same
redaction with `--raw`:

```
uv run --project ml python scripts/record_sse.py NAME --raw captured.txt
```

Conventions:

- name the file after the case it shows (`max_tokens`, `error_midstream`,
  `tool_use_parallel`), not after the prompt;
- keep the request that produced it under `fixtures/requests/` when the
  fixture is meant to be re-recorded after an API change;
- every text file, fixtures and `insta` snapshots included, is checked out
  with LF on every platform (`.gitattributes`): tests compare them byte
  for byte and prettier checks line endings;
- a hand-written fixture is fine when the documented event shape is all a
  test needs, but say so in the test.

## Continuous integration

`.github/workflows/ci.yml` runs on pushes to `main` and on pull requests:

- `check` on Windows, macOS and Linux: the same steps as `just check`
  (`cargo fmt --check`, `clippy -D warnings`, `build`, `test`; `tsc`,
  `eslint`, `prettier`, `vitest`; `ruff`, `pytest`), with `HARNESS_HOME`
  pointing at a runner temp directory;
- `deny`: `cargo deny check` (advisories, licences, sources);
- `gui-build` on the three platforms: `pnpm build:app` bundles the release
  daemon as the Tauri sidecar and uploads the installer as a workflow
  artifact (`apprentice-harness-<os>`: MSI and NSIS on Windows, DMG on
  macOS, deb, rpm and AppImage on Linux).

Rust builds are cached per job with `Swatinem/rust-cache`, pnpm and uv
through their setup actions. The Linux jobs install the WebKitGTK
development packages Tauri needs.

## Dependencies

`deny.toml` enforces the RustSec advisory database (yanked crates too),
the licence allow-list and crates.io as the only source. Unmaintained
advisories that reach us only through Tauri are listed under
`[advisories].ignore` with the path that pulls them in; vulnerabilities
are never ignored — update the dependency instead, raising the workspace
`rust-version` when the fix needs a newer compiler. The licence list is
what the current tree needs and is finalised together with the project
licence.

## Task workflow

Work is organised in task files under `tasks/` (format and rules in
[tasks/README.md](tasks/README.md)). A task's implementation starts with the
task id and title, follows its Design section, ticks its Acceptance
checklist, and ends with dated completion notes in the same file that
record what was built, what was decided differently from the design and
why, and what remains open. Commits reference the task id in the subject
(`... (M00-12)`).
