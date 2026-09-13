//! The tool system (task M01-01): how tools are declared, registered,
//! validated, executed, limited and recorded, independent of any
//! particular tool.
//!
//! A tool is a [`Tool`]: a [`ToolSpec`] (name, mentor-facing description,
//! JSON Schema of its input, risk class) and an async `call`. Tools live
//! in a [`ToolRegistry`], which is what the mentor's `tools` array is
//! built from (sorted by name, so the cached prefix is stable). The
//! runtime hands every `tool_use` block to an [`Executor`], which
//! validates the input, records `tool.call`, asks the [`Gate`]
//! (the permission engine of M01-07 plugs in there), runs the tool under
//! a timeout and the agent's cancellation token, captures the raw output
//! as a blob, cuts what the mentor sees down to `tools.max_mentor_bytes`
//! and records `tool.result`. [`Executor::execute_all`] applies the
//! parallel policy: read-only calls run concurrently, calls that change
//! things run one after another, results come back in the order the
//! mentor asked.
//!
//! The file tools (M01-03) live in [`file`], the search tool (M01-04)
//! in [`grep`], the shell tools (M01-05) in [`shell`], the git tools
//! (M01-06) in [`git`]; the loop that drives this is M01-08.
//! [`builtin_tools`] is the lot.

mod execute;
pub mod file;
pub mod git;
pub mod grep;
mod limits;
mod registry;
pub(crate) mod rpc;
mod schema;
pub mod shell;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use apprentice_api::events::Risk;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use execute::{AllowAll, Executed, Executor, Gate, ToolCall, ToolResultKind};
pub use file::file_tools;
pub use git::git_tools;
pub use grep::search_tools;
pub use limits::{OUTPUT_MEDIA_TYPE, Truncated, truncate_utf8};
pub use registry::{RegistryError, ToolRegistry};
pub use rpc::ToolsService;
pub use schema::{ToolValidator, is_valid_name};
pub use shell::shell_tools;

use crate::config::ToolsConfig;
use crate::trace::{AgentId, SessionId};
use crate::workspace::Workspace;

/// Longest `summary` line; longer ones are cut by the executor.
pub const MAX_SUMMARY_CHARS: usize = 120;

/// Every built-in tool, for [`ToolRegistry::register_all`].
pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    let mut tools = file_tools();
    tools.extend(search_tools());
    tools.extend(shell_tools());
    tools.extend(git_tools());
    tools
}

/// A tool the mentor (and later the apprentice) can call.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Static description of the tool. Called once at registration.
    fn spec(&self) -> ToolSpec;

    /// Runs the tool. `input` has already been validated against
    /// `spec().input_schema`. `cancel` fires when the agent is cancelled
    /// or the daemon shuts down; a tool that ignores it is abandoned by
    /// the executor, so tools with side effects should honour it.
    ///
    /// # Errors
    /// See [`ToolError`]. A tool that ran but "failed" from the mentor's
    /// point of view (compile error, missing file) should return `Ok`
    /// with `is_error: true` and the diagnostic as content, so the mentor
    /// sees the detail.
    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}

/// What the registry and the mentor see of a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// `snake_case`, unique in the registry.
    pub name: String,
    /// Written for the mentor: what the tool does, when to use it, what
    /// the arguments mean.
    pub description: String,
    /// JSON Schema (draft 2020-12) of `input`.
    pub input_schema: Value,
    pub risk: Risk,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Overrides the per-risk default timeout from config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

impl ToolSpec {
    /// A spec with no tags and the default timeout.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        risk: Risk,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            risk,
            tags: Vec::new(),
            timeout_s: None,
        }
    }

    #[must_use]
    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_s = Some(timeout.as_secs().max(1));
        self
    }

    /// `true` for the risk classes that may change state: such calls
    /// never run concurrently with each other.
    pub fn is_mutating(&self) -> bool {
        risk_is_mutating(self.risk)
    }
}

/// `Write` and `Execute` change the workspace; `ReadOnly` and `Network`
/// do not (a network tool that writes files declares `Write`).
pub fn risk_is_mutating(risk: Risk) -> bool {
    matches!(risk, Risk::Write | Risk::Execute)
}

/// Limits of one call, resolved from config and the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEnv {
    pub timeout: Duration,
    /// Longest raw output stored whole in the trace.
    pub max_capture_bytes: usize,
    /// Longest result text the mentor receives.
    pub max_mentor_bytes: usize,
}

impl ToolEnv {
    /// Per-risk timeout from `config`, overridden by the spec.
    pub fn resolve(config: &ToolsConfig, spec: &ToolSpec) -> Self {
        let secs = spec.timeout_s.unwrap_or(match spec.risk {
            Risk::ReadOnly => config.timeout_s.read_only,
            Risk::Write => config.timeout_s.write,
            Risk::Network => config.timeout_s.network,
            // `Risk` is non-exhaustive; anything newer is treated like
            // the slowest class.
            Risk::Execute | _ => config.timeout_s.execute,
        });
        Self {
            timeout: Duration::from_secs(secs.max(1)),
            max_capture_bytes: usize::try_from(config.max_capture_bytes).unwrap_or(usize::MAX),
            max_mentor_bytes: usize::try_from(config.max_mentor_bytes).unwrap_or(usize::MAX),
        }
    }
}

impl Default for ToolEnv {
    fn default() -> Self {
        Self::resolve(
            &ToolsConfig::default(),
            &ToolSpec::new("", "", Value::Null, Risk::ReadOnly),
        )
    }
}

/// Where a call runs. One per call; `call_id` is the mentor's
/// `tool_use` id.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// The session's workspace (path sandbox, ignore rules, index), when
    /// it has one. Tools resolve every path through it.
    pub workspace: Option<Arc<Workspace>>,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub call_id: String,
    pub env: ToolEnv,
    /// The session's `tools.*` config, for tools with settings of their
    /// own (`tools.shell`).
    pub config: Arc<ToolsConfig>,
    /// Files this agent has read, so a write can say when it overwrites
    /// something the mentor never looked at. Shared across the agent's
    /// steps by the runtime.
    pub seen: Arc<SeenFiles>,
    /// Partial output for long tools (shell). Dropped receivers are fine:
    /// sends fail silently.
    pub progress: mpsc::Sender<ToolProgress>,
}

impl ToolContext {
    /// Sends a progress chunk; does nothing when nobody listens.
    pub fn report(&self, stream: ProgressStream, text: impl Into<String>) {
        let _ = self.progress.try_send(ToolProgress {
            call_id: self.call_id.clone(),
            stream,
            text: text.into(),
        });
    }
}

/// The files one agent has read or written, with the SHA-256 of their
/// content at that moment. `write_file`/`edit_file` compare against it
/// to note an overwrite of unread or since-changed content; nothing
/// here blocks a call (permissions do that).
#[derive(Debug, Default)]
pub struct SeenFiles {
    inner: Mutex<HashMap<PathBuf, String>>,
}

impl SeenFiles {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, String>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that `path` (absolute) was seen with content hash `hash`.
    pub fn record(&self, path: &Path, hash: impl Into<String>) {
        self.lock().insert(path.to_path_buf(), hash.into());
    }

    /// The content hash `path` had when last seen.
    pub fn hash_of(&self, path: &Path) -> Option<String> {
        self.lock().get(path).cloned()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.lock().contains_key(path)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// A chunk of partial output from a running tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProgress {
    pub call_id: String,
    pub stream: ProgressStream,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStream {
    Stdout,
    Stderr,
}

/// What a tool produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: ToolContent,
    /// One line, at most [`MAX_SUMMARY_CHARS`], for cards and the trace:
    /// "read 212 lines of src/main.rs", "exit 1 in 3.2 s". Empty means
    /// "let the executor make one".
    pub summary: String,
    /// Structured facts about the run (exit code, match counts); stored
    /// in the `tool.result` payload.
    pub metadata: Value,
    /// The tool ran but the mentor should treat the result as a failure.
    pub is_error: bool,
    /// Further raw outputs stored as blobs of their own and listed in
    /// the `tool.result` payload (`shell` keeps stdout and stderr apart
    /// this way). The mentor never sees them.
    pub attachments: Vec<Attachment>,
}

/// A named side output of a call; see [`ToolOutput::attachments`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: ToolContent::Text(text.into()),
            summary: String::new(),
            metadata: Value::Null,
            is_error: false,
            attachments: Vec::new(),
        }
    }

    pub fn json(value: Value) -> Self {
        Self {
            content: ToolContent::Json(value),
            summary: String::new(),
            metadata: Value::Null,
            is_error: false,
            attachments: Vec::new(),
        }
    }

    /// A failure the mentor should see, with the diagnostic as content.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text)
        }
    }

    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    #[must_use]
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Adds a text attachment (stored as [`OUTPUT_MEDIA_TYPE`]).
    #[must_use]
    pub fn with_text_attachment(mut self, name: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.attachments.push(Attachment {
            name: name.into(),
            media_type: OUTPUT_MEDIA_TYPE.to_owned(),
            bytes,
        });
        self
    }
}

/// The content of a result. `Binary` is for screenshots and the like
/// (vision, M10); the mentor gets a placeholder for it in M01.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolContent {
    Text(String),
    Json(Value),
    Binary { media_type: String, bytes: Vec<u8> },
}

impl ToolContent {
    /// The raw bytes stored as the `tool.result` blob and their media
    /// type. JSON is serialised compactly.
    pub fn to_bytes(&self) -> (Vec<u8>, &str) {
        match self {
            Self::Text(t) => (t.as_bytes().to_vec(), OUTPUT_MEDIA_TYPE),
            Self::Json(v) => (
                serde_json::to_vec(v).unwrap_or_default(),
                "application/json",
            ),
            Self::Binary { media_type, bytes } => (bytes.clone(), media_type),
        }
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Self::Binary { .. })
    }
}

/// Why a call produced no [`ToolOutput`].
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The input did not match the schema, or a value was unusable.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The permission engine (or the tool itself) refused.
    #[error("denied: {0}")]
    Denied(String),
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The tool could not run at all (as opposed to running and
    /// reporting an error result).
    #[error("{0}")]
    Failed(String),
}

impl ToolError {
    /// Stable kind for the trace (`tool.result.error_kind`) and events.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Denied(_) => "denied",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Io(_) => "io",
            Self::Failed(_) => "failed",
        }
    }
}

impl fmt::Display for ProgressStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_resolves_per_risk_with_spec_override() {
        let config = ToolsConfig::default();
        let spec = |risk| ToolSpec::new("t", "d", Value::Null, risk);
        assert_eq!(
            ToolEnv::resolve(&config, &spec(Risk::ReadOnly)).timeout,
            Duration::from_secs(30)
        );
        assert_eq!(
            ToolEnv::resolve(&config, &spec(Risk::Execute)).timeout,
            Duration::from_secs(600)
        );
        assert_eq!(
            ToolEnv::resolve(&config, &spec(Risk::Network)).timeout,
            Duration::from_secs(120)
        );
        let custom = spec(Risk::Execute).with_timeout(Duration::from_secs(5));
        let env = ToolEnv::resolve(&config, &custom);
        assert_eq!(env.timeout, Duration::from_secs(5));
        assert_eq!(env.max_capture_bytes, 8 * 1024 * 1024);
        assert_eq!(env.max_mentor_bytes, 32 * 1024);
    }

    #[test]
    fn mutating_risks() {
        assert!(!risk_is_mutating(Risk::ReadOnly));
        assert!(!risk_is_mutating(Risk::Network));
        assert!(risk_is_mutating(Risk::Write));
        assert!(risk_is_mutating(Risk::Execute));
    }

    #[test]
    fn content_bytes() {
        let text = ToolContent::Text("hi".into());
        let (b, m) = text.to_bytes();
        assert_eq!((b.as_slice(), m), (b"hi".as_slice(), OUTPUT_MEDIA_TYPE));
        let json = ToolContent::Json(serde_json::json!({"a": 1}));
        let (b, m) = json.to_bytes();
        assert_eq!(
            (b.as_slice(), m),
            (br#"{"a":1}"#.as_slice(), "application/json")
        );
    }
}
