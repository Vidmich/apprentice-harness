# apprentice-harness GUI

Tauri 2 + React + Vite + TypeScript. The Rust side (`src-tauri`) is a thin
client of the daemon through `apprentice-client`; it never links
`apprentice-core`.

## Prerequisites (Windows)

- Rust stable (see `../../rust-toolchain.toml`), MSVC build tools
- WebView2 runtime (bundled with Windows 11)
- Node 22+, pnpm 10+

## Commands

```
pnpm install
pnpm tauri dev      # dev window with HMR
pnpm tauri build    # installer under src-tauri/target/release/bundle
pnpm lint / pnpm test / pnpm typecheck
```

## Sidecar

From M00-10 the daemon is bundled as a sidecar. Tauri requires the binary to be
named `binaries/harnessd-<target-triple>[.exe]`; `just gui-sidecar` copies the
built daemon into place.
