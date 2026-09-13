//! Configuration schema v1. Every field has a default so an empty file (or
//! no file) is a valid configuration; unknown keys are rejected.

use std::collections::BTreeMap;

use apprentice_api::types::{Effort, PermissionMode};
use serde::{Deserialize, Serialize};

/// The resolved configuration.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub mentor: MentorConfig,
    /// USD per million tokens, keyed by model id. Editable so users can
    /// correct prices without a release (consumed by token accounting).
    pub pricing: BTreeMap<String, Pricing>,
    pub trace: TraceConfig,
    pub daemon: DaemonConfig,
    pub apprentice: ApprenticeConfig,
    pub permissions: PermissionsConfig,
    pub tools: ToolsConfig,
    pub runtime: RuntimeConfig,
    pub sessions: SessionsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MentorConfig {
    pub provider: String,
    pub model: String,
    /// The cheap model that names sessions (task M01-10).
    pub title_model: String,
    pub effort: Effort,
    pub max_tokens: u32,
    pub thinking_display: ThinkingDisplay,
    pub base_url: String,
    pub timeout_s: u64,
    pub max_retries: u32,
    /// Input tokens of a call (prompt plus cache reads) past which the
    /// agent warns that the context is large (task M01-08).
    pub context_soft_limit: u64,
    /// Input tokens past which the agent stops with `context_limit`
    /// rather than call again.
    pub context_hard_limit: u64,
}

impl Default for MentorConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
            title_model: "claude-haiku-4-5-20251001".into(),
            effort: Effort::High,
            max_tokens: 64_000,
            thinking_display: ThinkingDisplay::Summarized,
            base_url: "https://api.anthropic.com".into(),
            timeout_s: 600,
            max_retries: 4,
            context_soft_limit: 600_000,
            context_hard_limit: 900_000,
        }
    }
}

/// The agent loop (task M01-08). Workspace-overridable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    /// Mentor calls one `agent.run` may make before it stops with
    /// `max_iterations`.
    pub max_iterations: u32,
    /// Longest wait, in seconds, for a rate limit or an overload to
    /// pass (beyond the adapter's own retries) before the run fails.
    pub max_wait_s: u64,
    /// The stalled-agent watchdog (task M01-15): a run that produces
    /// no event for the longest configured wait (`mentor.timeout_s`,
    /// `tools.timeout_s.execute`, `permissions.ask_timeout_s`) plus
    /// this many seconds is ended with `error: stalled`. `0` turns the
    /// watchdog off.
    pub stall_grace_s: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_iterations: 200,
            max_wait_s: 300,
            stall_grace_s: 60,
        }
    }
}

/// Session bookkeeping (task M01-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionsConfig {
    /// After a session's first answer, ask `mentor.title_model` for a
    /// short title (unless the user set one).
    pub auto_title: bool,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self { auto_title: true }
    }
}

/// How the mentor's thinking is shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
}

/// USD per million tokens. Cache prices are optional and default to the
/// first-party convention (reads 10% of input, writes 125% of input).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl Pricing {
    pub const fn first_party(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_read: Some(input * 0.1),
            cache_write: Some(input * 1.25),
        }
    }

    pub fn cache_read(&self) -> f64 {
        self.cache_read.unwrap_or(self.input * 0.1)
    }

    pub fn cache_write(&self) -> f64 {
        self.cache_write.unwrap_or(self.input * 1.25)
    }
}

/// Built-in price table (verify against the pricing page when updating).
pub fn default_pricing() -> BTreeMap<String, Pricing> {
    BTreeMap::from([
        ("claude-opus-5".to_owned(), Pricing::first_party(5.0, 25.0)),
        (
            "claude-sonnet-5".to_owned(),
            Pricing::first_party(2.0, 10.0),
        ),
        (
            "claude-haiku-4-5-20251001".to_owned(),
            Pricing::first_party(1.0, 5.0),
        ),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TraceConfig {
    /// Store the raw SSE stream of every mentor call as a blob.
    pub capture_raw_sse: bool,
    /// Payload strings above this size are moved to blobs.
    pub inline_payload_max_bytes: u64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            capture_raw_sse: false,
            inline_payload_max_bytes: 4096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// `tracing` filter directive, e.g. `info` or `apprentice_core=debug`.
    pub log_level: String,
    /// Minutes without clients before the daemon exits; 0 = never.
    pub idle_shutdown_min: u64,
    /// Where secrets are persisted when not provided by the environment.
    pub secret_store: SecretStoreKind,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
            idle_shutdown_min: 0,
            secret_store: SecretStoreKind::Keychain,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStoreKind {
    /// OS keychain (Credential Manager / Keychain Services / Secret Service).
    Keychain,
    /// `<data_dir>/secrets.toml`, for headless hosts without a keyring.
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprenticeConfig {
    /// Flipped on once local inference exists.
    pub enabled: bool,
}

/// The permission engine (task M01-07). Workspace-overridable. The
/// rules themselves live in `permissions.toml` next to the config file
/// and in `<workspace>/.harness/permissions.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionsConfig {
    /// The `permission_mode` of a run that does not set one: `default`
    /// (rules decide, the client is asked for the rest), `plan`
    /// (writes and commands denied) or `auto` (writes inside the
    /// workspace allowed without asking).
    pub default_mode: PermissionMode,
    /// How long a prompt waits for an answer before the call is denied.
    pub ask_timeout_s: u64,
    /// What happens to a call that would be asked when no client is
    /// attached to the agent.
    pub headless: Headless,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default_mode: PermissionMode::Default,
            ask_timeout_s: 600,
            headless: Headless::Deny,
        }
    }
}

/// `permissions.headless`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Headless {
    /// Deny everything that would have been asked.
    #[default]
    Deny,
    /// Allow read-only calls, deny the rest.
    AllowReadonly,
}

/// Limits of the tool system (task M01-01). Workspace-overridable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// Tool names the mentor is not offered (`tools.list` reports them as
    /// disabled).
    pub disabled: Vec<String>,
    /// Longest raw output captured in the trace; beyond it the blob keeps
    /// the head and the tail and the result is marked
    /// `truncated_at_capture`.
    pub max_capture_bytes: u64,
    /// Longest tool result the mentor receives (head + tail with an
    /// omission marker); the trace keeps the whole output.
    pub max_mentor_bytes: u64,
    /// Default timeouts in seconds by risk class; a tool spec may override.
    pub timeout_s: ToolTimeouts,
    pub shell: ShellConfig,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            disabled: Vec::new(),
            max_capture_bytes: 8 * 1024 * 1024,
            max_mentor_bytes: 32 * 1024,
            timeout_s: ToolTimeouts::default(),
            shell: ShellConfig::default(),
        }
    }
}

/// The `shell` tool (task M01-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShellConfig {
    /// The program that runs a command. Empty picks one per OS: `pwsh`
    /// (else `powershell`) on Windows, `$SHELL` (else `/bin/sh`)
    /// elsewhere.
    pub program: String,
    /// Its arguments before the command. Empty means the program's own
    /// defaults (`-NoProfile -NonInteractive -Command` for PowerShell,
    /// `-lc` for a Unix shell); a configured `program` with no `args`
    /// gets the command as its only argument.
    pub args: Vec<String>,
    /// Environment variables a command never sees, as names or `*`
    /// patterns (matched case-insensitively).
    pub scrub_env: Vec<String>,
    /// Variables set for every command, on top of `HARNESS=1`,
    /// `NO_COLOR=1` and `TERM=dumb` (e.g. `CI = "1"`).
    pub env: BTreeMap<String, String>,
    /// Longest `timeout_s` a call may ask for.
    pub max_timeout_s: u64,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            scrub_env: [
                "ANTHROPIC_API_KEY",
                "*_API_KEY",
                "*_TOKEN",
                "*_SECRET",
                "*_SECRET_*",
                "*_PASSWORD",
                "AWS_*",
            ]
            .map(str::to_owned)
            .to_vec(),
            env: BTreeMap::new(),
            max_timeout_s: 3600,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolTimeouts {
    pub read_only: u64,
    pub write: u64,
    pub execute: u64,
    pub network: u64,
}

impl Default for ToolTimeouts {
    fn default() -> Self {
        Self {
            read_only: 30,
            write: 30,
            execute: 600,
            network: 120,
        }
    }
}

impl Config {
    /// Built-in defaults, including the price table.
    pub fn builtin() -> Self {
        Self {
            pricing: default_pricing(),
            ..Self::default()
        }
    }
}

/// Dotted-key prefixes a workspace config may set. Everything else is
/// rejected with `KeyNotOverridable`.
pub const WORKSPACE_OVERRIDABLE: &[&str] = &[
    "mentor.model",
    "mentor.effort",
    "mentor.max_tokens",
    "apprentice.",
    "permissions.",
    "tools.",
    "runtime.",
];

/// Whether a dotted leaf key may appear in a workspace config.
pub fn workspace_overridable(key: &str) -> bool {
    WORKSPACE_OVERRIDABLE.iter().any(|allowed| {
        if let Some(prefix) = allowed.strip_suffix('.') {
            key.starts_with(prefix) && key[prefix.len()..].starts_with('.')
        } else {
            key == *allowed
        }
    })
}

/// Environment overrides: `(variable, dotted key)`.
pub const ENV_OVERRIDES: &[(&str, &str)] = &[
    ("HARNESS_MENTOR_MODEL", "mentor.model"),
    ("HARNESS_MENTOR_EFFORT", "mentor.effort"),
    ("HARNESS_MENTOR_BASE_URL", "mentor.base_url"),
    ("HARNESS_LOG_LEVEL", "daemon.log_level"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let c = Config::builtin();
        let text = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
        assert!(text.contains("model = \"claude-opus-5\""));
        assert!(text.contains("[pricing.claude-opus-5]"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<Config>("[mentor]\nmodle = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("modle"), "{err}");
    }

    #[test]
    fn workspace_whitelist() {
        assert!(workspace_overridable("mentor.model"));
        assert!(workspace_overridable("apprentice.enabled"));
        assert!(workspace_overridable("permissions.default_mode"));
        assert!(workspace_overridable("tools.disabled"));
        assert!(!workspace_overridable("mentor.base_url"));
        assert!(!workspace_overridable("apprentice"));
        assert!(!workspace_overridable("apprenticex.enabled"));
        assert!(!workspace_overridable("daemon.log_level"));
    }

    #[test]
    fn pricing_convention() {
        let p = default_pricing()["claude-opus-5"];
        assert!((p.cache_read() - 0.5).abs() < 1e-9);
        assert!((p.cache_write() - 6.25).abs() < 1e-9);
        let partial: Pricing = toml::from_str(
            "input = 2.0
output = 4.0
",
        )
        .unwrap();
        assert!((partial.cache_read() - 0.2).abs() < 1e-9);
        assert!((partial.cache_write() - 2.5).abs() < 1e-9);
    }
}
