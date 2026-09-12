# M00-03 — Layered configuration and secrets

Status: done
Depends on: M00-01
Size: S

## Goal

`apprentice_core::config` loads a typed configuration from three layers
(built-in defaults → user `config.toml` → workspace `.harness/config.toml`),
reports where each value came from, supports get/set of dotted keys, and
stores secrets (the Anthropic API key) in the OS keychain rather than in files.

## Context

SPEC §15: config in TOML, layered, secrets in the OS keychain; §3.1 Config
component; `tasks/README.md` fixes the directory scheme and `HARNESS_HOME`.

## Scope

In: schema, loading/merging, provenance, validation, write-back to a layer,
keychain access, env-var overrides for headless use, RPC handlers for
`config.*` and `auth.*` (handlers are thin; wiring into the daemon is M00-08).
Out: GUI settings screen (M01), model manager config (M02).

## Design

### Locations

```
HARNESS_HOME (env, optional)  → <home>/config.toml and <home>/data/...
else ProjectDirs("", "", "apprentice-harness"):
  config_dir/config.toml
  data_dir/{traces.sqlite, blobs/, logs/, models/, daemon.json, daemon.lock}
workspace: <workspace_root>/.harness/config.toml   (only a whitelisted subset of keys allowed)
```

### Schema (v1) — `struct Config` with serde defaults

```toml
[mentor]
provider   = "anthropic"
model      = "claude-opus-5"
effort     = "high"            # low|medium|high|xhigh|max
max_tokens = 64000
thinking_display = "summarized"   # summarized|omitted
base_url   = "https://api.anthropic.com"
timeout_s  = 600
max_retries = 4

[pricing."claude-opus-5"]      # USD per million tokens; editable, see M00-07
input = 5.0
output = 25.0
cache_read = 0.5               # verify against the pricing page; defaults assume 10% of input
cache_write = 6.25             # 1.25 x input

[trace]
capture_raw_sse = false        # store raw SSE stream blobs for debugging
inline_payload_max_bytes = 4096

[daemon]
log_level = "info"
idle_shutdown_min = 0          # 0 = never

[apprentice]
enabled = false                # flipped in M03

[permissions]                  # M01 fills this in; reserve the table now
default_mode = "ask"
```

Workspace-overridable keys: `mentor.model`, `mentor.effort`,
`mentor.max_tokens`, `apprentice.*`, `permissions.*`. Anything else in a
workspace file is rejected with `ConfigError::KeyNotOverridable`.

Env overrides (headless/CI): `HARNESS_MENTOR_MODEL`, `HARNESS_MENTOR_EFFORT`,
`HARNESS_MENTOR_BASE_URL`, `HARNESS_LOG_LEVEL`. `ANTHROPIC_API_KEY` is honoured
as a secret source *before* the keychain (so tests and CI never touch the
keychain).

### API

```rust
pub struct ConfigLoader { home: Paths }
impl ConfigLoader {
    pub fn load(&self, workspace: Option<&Path>) -> Result<Resolved, ConfigError>;
}
pub struct Resolved { pub config: Config, pub sources: BTreeMap<String /*dotted key*/, Source> }
pub enum Source { Default, User, Workspace, Env }
pub fn get(resolved: &Resolved, key: &str) -> Option<(serde_json::Value, Source)>;
pub fn set(paths: &Paths, layer: Layer, workspace: Option<&Path>, key: &str, value: serde_json::Value) -> Result<(), ConfigError>;
```

`set` edits the TOML file preserving comments and order (`toml_edit`), creates
the file if missing, validates by reloading, and rejects unknown keys.

### Secrets

```rust
pub trait SecretStore { fn get(&self, name: &str) -> Result<Option<String>>; fn set(&self, name: &str, value: &str) -> Result<()>; fn delete(&self, name: &str) -> Result<()>; }
KeychainStore  → keyring crate, service "apprentice-harness", user = secret name ("anthropic_api_key")
EnvStore       → ANTHROPIC_API_KEY
FileStore      → <data_dir>/secrets.toml with 0600 perms; only when config.daemon.secret_store = "file" (headless Linux without a keyring daemon)
ChainStore     → Env, then Keychain (or File)
```

`auth.status` reports `configured: true/false` per provider and the source
(`env`/`keychain`/`file`) but never the value. Logging must never print
secrets: wrap in a `Secret(String)` newtype whose `Debug` prints `***`.

## Acceptance

- [x] Defaults load with no files present; `config.path` reports the paths.
- [x] User and workspace layers merge with correct precedence; `sources`
      reports the winning layer per key; env override wins over files.
- [x] Workspace file with a non-overridable key fails with a clear error
      naming the key and file.
- [x] `set` round-trips through `toml_edit` preserving comments.
- [x] Secret set/get/delete works via keychain on Windows (Credential
      Manager) and via `FileStore`; `ANTHROPIC_API_KEY` env takes precedence.
- [x] `Secret` never appears in `Debug`/logs (test greps a captured log).

## Verification

Unit tests with `HARNESS_HOME` set to a temp dir covering every bullet
above. Keychain test is `#[ignore]` by default (needs a desktop session);
run manually once on Windows.

## Notes

- `keyring` crate v3 needs the platform feature flags (`windows-native`,
  `apple-native`, `sync-secret-service` or `linux-native`); pick them in the
  workspace manifest.
- Pricing values are only defaults; M00-07 consumes them. Keep them in config
  so users can correct them without a release.

## Completion notes (2026-09-12)

- `apprentice_core::config`: `Paths` (`HARNESS_HOME` or `ProjectDirs`),
  `Config` schema v1 with `deny_unknown_fields` and full defaults,
  `ConfigLoader` (env overrides injected via `with_env` so tests never touch
  the process environment; `with_process_env` for the daemon), `Resolved`
  with the merged JSON tree and per-leaf `sources`, `set` via `toml_edit`
  (keeps comments and trailing decor, writes atomically, validates the layer
  before writing, `null` removes a key, an object value sets a sub-table —
  needed for `pricing.<model>`), `ConfigService` with the five `config.*` /
  `auth.*` handlers and a `register(router)` helper (blocking work on the
  tokio blocking pool).
- `apprentice_core::secrets`: `Secret` (Debug → `***`, no Display/Serialize),
  `SecretStore` trait, `EnvStore` (process or fixed map), `KeychainStore`
  (`keyring` 4 default `v1` feature — no platform flags needed),
  `FileStore` (0600 on Unix), `ChainStore::lookup` reporting the source.
  `config::secret_store(paths, kind)` builds the chain from
  `daemon.secret_store`.
- Schema deviations from the task text: `Pricing.cache_read/cache_write` are
  optional (default 10% / 125% of input via accessors) so a new model needs
  only `input` and `output`; `daemon.secret_store = "keychain"|"file"` added.
- Tests: 14 unit + 10 integration (`crates/core/tests/config.rs`, incl. the
  RPC handlers over a router and a captured-`tracing`-log check for
  `Secret`). Keychain round trip is `#[ignore]` and was run manually on
  Windows Credential Manager: pass.
- Not done here: wiring `ConfigService` into `harnessd` (M00-08).
