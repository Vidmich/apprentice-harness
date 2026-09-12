//! Shared data types used by method params/results and events.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mentor effort level (`output_config.effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Effort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

/// Token usage of one mentor call, exactly as reported by the API.
/// Missing fields deserialise as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Where a configuration value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfigSource {
    Default,
    User,
    Workspace,
    Env,
}

/// Configuration layer a value is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfigLayer {
    User,
    Workspace,
}

/// Session row as listed by `session.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Trace event row as listed by `trace.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSummary {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub step_id: Option<String>,
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    /// Size of the attached blob, if any.
    #[serde(default)]
    pub blob_bytes: Option<u64>,
}

/// Full trace event as returned by `trace.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceEvent {
    #[serde(flatten)]
    pub summary: EventSummary,
    pub payload: Value,
    #[serde(default)]
    pub blob_id: Option<String>,
}

/// Options for `agent.run`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// `None` = use config (`apprentice.enabled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apprentice: Option<bool>,
}

/// Aggregated token statistics (see task M00-07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenStats {
    pub range: StatsRange,
    /// IANA name or fixed offset of the timezone used for `by_day`.
    pub tz: String,
    pub totals: TokenBucket,
    #[serde(default)]
    pub by_model: Vec<TokenBucket>,
    #[serde(default)]
    pub by_day: Vec<TokenBucket>,
    #[serde(default)]
    pub by_session: Vec<TokenBucket>,
    pub apprentice: ApprenticeStats,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsRange {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
}

/// One row of aggregated usage. The `key` names the group (model id, day,
/// session id) and is absent for the grand total.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TokenBucket {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost_usd: f64,
    /// Calls whose model had no pricing entry (their cost is excluded).
    #[serde(default)]
    pub unpriced_calls: u64,
}

/// Apprentice-side counters (filled from M03 on; zero until then).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApprenticeStats {
    pub invocations: u64,
    pub bypassed: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub estimated_saved_input: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_wire_names() {
        assert_eq!(serde_json::to_string(&Effort::XHigh).unwrap(), "\"xhigh\"");
        assert_eq!(
            serde_json::from_str::<Effort>("\"max\"").unwrap(),
            Effort::Max
        );
    }

    #[test]
    fn usage_missing_fields_default_to_zero() {
        let u: Usage = serde_json::from_str(r#"{"input_tokens": 12}"#).unwrap();
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.cache_read_input_tokens, 0);
    }
}

/// A tool as `tools.list` reports it (task M01-01).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    /// The text the mentor sees.
    pub description: String,
    /// JSON Schema (draft 2020-12) of the tool's input.
    pub input_schema: Value,
    pub risk: crate::events::Risk,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Timeout override in seconds; absent = the per-risk default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
    /// `false` when `tools.disabled` names it: not offered to the mentor.
    pub enabled: bool,
}
