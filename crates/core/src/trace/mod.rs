//! Trace store: the append-only record of everything an agent does.
//!
//! Sessions, agents, steps and typed events live in
//! `<data_dir>/traces.sqlite` (WAL); large payloads are content-addressed
//! blobs under `<data_dir>/blobs/<aa>/<sha256hex>`. Any mentor request can
//! be reconstructed byte-for-byte from its `mentor.request` blob, which is
//! what the replay evaluator needs.
//!
//! [`TraceStore`] is the synchronous store (a `Mutex<Connection>`);
//! [`TraceWriter`] runs it on a dedicated thread behind a bounded queue so
//! the agent loop never blocks on disk. [`TraceService`] exposes the query
//! side over RPC.

mod blobs;
mod error;
mod payload;
mod record;
mod rpc;
mod schema;
mod store;
mod writer;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use blobs::sha256_hex;
pub use error::TraceError;
pub use payload::{BLOB_REF_KEY, blob_refs};
pub use record::StepRef;
pub use rpc::TraceService;
pub use schema::SCHEMA_VERSION;
pub use store::{
    AgentRecord, BlobInput, BlobMeta, CallFilter, DiskUsage, EventQuery, GroupBy, GroupedTotals,
    IntegrityReport, MAX_PAGE, MentorCallEnd, MentorCallRow, MentorCallStart, NewAgent, NewEvent,
    NewSession, RepriceReport, SessionRecord, StepRecord, TraceStore, UsageTotals, WorkspaceRecord,
};
pub use writer::{MAX_BATCH, QUEUE_CAPACITY, TraceWriter};

/// Database file name under the data directory.
pub const DB_FILE: &str = "traces.sqlite";
/// Blob directory name under the data directory.
pub const BLOB_DIR: &str = "blobs";

/// Event kinds of schema v1. Kinds are free-form strings on the wire (later
/// milestones add more) but the runtime uses these constants.
pub mod kinds {
    pub const SESSION_CREATED: &str = "session.created";
    pub const AGENT_STARTED: &str = "agent.started";
    pub const AGENT_FINISHED: &str = "agent.finished";
    pub const USER_MESSAGE: &str = "user.message";
    pub const ASSISTANT_MESSAGE: &str = "assistant.message";
    pub const MENTOR_REQUEST: &str = "mentor.request";
    pub const MENTOR_RESPONSE: &str = "mentor.response";
    pub const MENTOR_ERROR: &str = "mentor.error";
    pub const TOOL_CALL: &str = "tool.call";
    pub const TOOL_RESULT: &str = "tool.result";
    pub const APPRENTICE_INVOCATION: &str = "apprentice.invocation";
    /// Opaque apprentice state snapshot (KV cache or recurrent state); the
    /// blob may be pruned by retention while the row stays.
    pub const APPRENTICE_STATE: &str = "apprentice.state";
    pub const PERMISSION_DECISION: &str = "permission.decision";
    pub const OUTCOME: &str = "outcome";
    pub const FEEDBACK: &str = "feedback";
    pub const WORKSPACE_SNAPSHOT: &str = "workspace.snapshot";

    pub const ALL: &[&str] = &[
        SESSION_CREATED,
        AGENT_STARTED,
        AGENT_FINISHED,
        USER_MESSAGE,
        ASSISTANT_MESSAGE,
        MENTOR_REQUEST,
        MENTOR_RESPONSE,
        MENTOR_ERROR,
        TOOL_CALL,
        TOOL_RESULT,
        APPRENTICE_INVOCATION,
        APPRENTICE_STATE,
        PERMISSION_DECISION,
        OUTCOME,
        FEEDBACK,
        WORKSPACE_SNAPSHOT,
    ];

    /// `name` or `area.name`, lower-case ASCII letters, digits and
    /// underscores.
    pub fn is_valid(kind: &str) -> bool {
        let ok = |p: &str| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        let mut parts = kind.split('.');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(a), None, _) => ok(a),
            (Some(a), Some(b), None) => ok(a) && ok(b),
            _ => false,
        }
    }
}

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                v: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                String::column_result(v).map(Self)
            }
        }
    };
}

macro_rules! generated_id {
    ($(#[$m:meta])* $name:ident) => {
        id_type!($(#[$m])* $name);

        impl $name {
            /// A fresh time-ordered (UUID v7) id.
            pub fn generate() -> Self {
                Self(uuid::Uuid::now_v7().to_string())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::generate()
            }
        }
    };
}

generated_id!(SessionId);
generated_id!(AgentId);
generated_id!(StepId);
generated_id!(EventId);
generated_id!(
    /// Id of one mentor call, shared by its request/response/error events
    /// and the `mentor_calls` row.
    CallId
);
generated_id!(
    /// Id of a registered workspace (`workspaces` row, schema v2).
    WorkspaceId
);
id_type!(
    /// SHA-256 hex of the blob content.
    BlobId
);

/// Lifecycle status shared by agents, steps and mentor calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Ok,
    Cancelled,
    Error,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "ok" => Self::Ok,
            "cancelled" => Self::Cancelled,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Main,
    Sub,
    Eval,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Sub => "sub",
            Self::Eval => "eval",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "main" => Self::Main,
            "sub" => Self::Sub,
            "eval" => Self::Eval,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Open,
    Archived,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "archived" => Self::Archived,
            _ => return None,
        })
    }
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ` (fixed width, so string
/// order is time order).
pub fn now_ts() -> String {
    format_ts(time::OffsetDateTime::now_utc())
}

pub fn format_ts(t: time::OffsetDateTime) -> String {
    const FORMAT: &[time::format_description::BorrowedFormatItem<'_>] = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    t.to_offset(time::UtcOffset::UTC)
        .format(FORMAT)
        .expect("fixed format never fails")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_valid_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in kinds::ALL {
            assert!(kinds::is_valid(k), "{k}");
            assert!(seen.insert(*k), "duplicate {k}");
        }
        assert!(!kinds::is_valid("Mentor.Request"));
        assert!(!kinds::is_valid(""));
        assert!(!kinds::is_valid("a.b.c"));
        assert!(!kinds::is_valid("a."));
    }

    #[test]
    fn ids_are_time_ordered_and_transparent() {
        let a = EventId::generate();
        let b = EventId::generate();
        assert!(a < b);
        assert_eq!(serde_json::to_value(&a).unwrap(), a.as_str());
        assert_eq!(format!("{a:?}"), format!("EventId({a})"));
    }

    #[test]
    fn timestamps_have_fixed_width() {
        let t = time::macros::datetime!(2026-09-12 10:00:00 UTC);
        assert_eq!(format_ts(t), "2026-09-12T10:00:00.000Z");
        assert_eq!(now_ts().len(), 24);
    }
}
