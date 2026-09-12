# apprentice-harness

A system-agnostic desktop application (GUI + CLI) for agentic coding work with
a remote foundation model (the *mentor*), where a small local model (the
*apprentice*) does as much work as possible so that the dialog with the remote
model stays short and token-cheap — without lowering task success.

- [SPEC.md](SPEC.md) — what the system is and how it is designed
- [ROADMAP.md](ROADMAP.md) — milestones and exit criteria
- [tasks/README.md](tasks/README.md) — task files and the fixed decisions every task assumes

## Layout

```
crates/core      apprentice_core   runtime, tools, mentor adapter, traces, inference
crates/api       apprentice_api    JSON-RPC types shared by daemon and clients
crates/common    apprentice_common paths and logging setup shared by every process
crates/client    apprentice_client daemon discovery/spawn + typed RPC client
crates/daemon    harnessd          hosts the core, serves the RPC API
crates/cli       harness           command line interface
apps/gui         Tauri 2 + React + Vite + TypeScript desktop app
ml/              Python (uv) training / evaluation workspace
protocol/        mentor–apprentice prompt protocol versions (from M03)
```

## Prerequisites

- Rust stable (`rustup update stable`), MSVC build tools on Windows
- Node 22+ and pnpm (`npm install -g pnpm`)
- [uv](https://docs.astral.sh/uv/) for Python
- [just](https://github.com/casey/just) (`uv tool install rust-just`)
- Windows: WebView2 runtime (included in Windows 11)

## Getting started

```
just setup      # install frontend and Python dependencies
just check      # build + lint + test everything
just gui        # open the desktop app (dev mode)
just cli -- --version
```
