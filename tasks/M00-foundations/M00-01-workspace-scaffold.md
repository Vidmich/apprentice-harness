# M00-01 — Repository and workspace scaffold

Status: todo
Depends on: none
Size: S

## Goal

A buildable monorepo skeleton: a Rust workspace with the five crates, a Tauri
GUI app shell, a Python `ml/` workspace, shared tooling (formatting, lints,
task runner) and a top-level README. Every crate compiles and has one passing
test; `pnpm tauri dev` opens an empty window; `uv run apprentice-ml --version`
prints a version.

## Context

Implements the layout fixed in `tasks/README.md` and SPEC §3/§16. Everything
in M00 hangs off this skeleton, so names and paths chosen here are permanent.

## Scope

In: directory layout, crate manifests, workspace-level lints and dependency
versions, Tauri/React/Vite app template, Python project, `justfile`,
`.editorconfig`, `rustfmt.toml`, `.gitignore`, README.
Out: any real functionality (config, RPC, mentor) — later tasks.

## Design

Directory layout (must match `tasks/README.md` exactly):

```
Cargo.toml                       [workspace] members = crates/*; resolver = "3"
rust-toolchain.toml              channel = "stable"
rustfmt.toml                     edition = "2024", max_width = 100
clippy.toml / [workspace.lints]  clippy::all + pedantic (selected) as warn; deny(unsafe_code) in all crates except future llama binding
crates/core/     apprentice-core    lib apprentice_core
crates/api/      apprentice-api     lib apprentice_api
crates/client/   apprentice-client  lib apprentice_client
crates/daemon/   harnessd           bin harnessd
crates/cli/      harness            bin harness
apps/gui/        Tauri 2 app (pnpm create tauri-app --template react-ts), package name "apprentice-harness-gui", window title "apprentice-harness"
ml/              pyproject.toml (uv), package apprentice_ml, src layout, console script apprentice-ml
protocol/        .gitkeep
justfile         build, test, lint, fmt, gui, ml-test recipes
README.md        one paragraph + links to SPEC.md / ROADMAP.md / tasks/README.md
```

Workspace `[workspace.package]`: version `0.1.0`, edition `2024`, license
field left as `LicenseRef-TBD` (licence is a deferred decision).
`[workspace.dependencies]` pins the shared crates listed in `tasks/README.md`
so all members use one version.

Crate dependency direction (enforced by review, keep acyclic):
`api` ← `client` ← `cli`, `gui`; `api` ← `core` ← `daemon`. `cli` and `gui`
must NOT depend on `core` (they must never link inference libraries).

Each lib crate exposes `pub const VERSION: &str = env!("CARGO_PKG_VERSION");`
and has one unit test. Each bin prints `<name> <version>` for `--version`.

GUI: keep the template's `src/` but strip demo content; add
`src/lib/rpc.ts` placeholder. Tauri config: app identifier
`dev.apprentice-harness.gui`, single window 1200×800, `withGlobalTauri: false`.
Frontend tooling: pnpm, TypeScript strict, ESLint + Prettier defaults.

Python: `ml/pyproject.toml` with `requires-python = ">=3.12"`, deps empty for
now (torch etc. added in M05/M06), dev deps `pytest`, `ruff`. Entry point
`apprentice_ml.cli:main` printing the version. `uv.lock` committed.

`justfile` recipes: `build` (cargo build --workspace), `test` (cargo test
--workspace + `uv run pytest` in ml + `pnpm test` in gui if present), `lint`
(cargo clippy --workspace --all-targets -D warnings, ruff, eslint), `fmt`,
`gui` (pnpm --dir apps/gui tauri dev), `daemon` (cargo run -p harnessd).

## Acceptance

- [ ] `cargo build --workspace` and `cargo test --workspace` pass on Windows,
      macOS and Linux (at least Windows verified locally now).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `pnpm --dir apps/gui install && pnpm --dir apps/gui tauri build` produces
      a binary; `tauri dev` opens a window titled "apprentice-harness".
- [ ] `uv run --directory ml apprentice-ml --version` prints the version.
- [ ] `just lint` and `just test` succeed.
- [ ] README links to SPEC, ROADMAP and tasks.

## Verification

Run the commands above. Add a CI-style script `scripts/check.ps1` and
`scripts/check.sh` that run build+test+lint for local use.

## Notes

- Tauri 2 on Windows needs WebView2 (present on Windows 11) and the MSVC
  toolchain; document prerequisites in README (Rust, pnpm, uv, Tauri CLI).
- Do not add llama.cpp or torch yet; keep the scaffold fast to build.
