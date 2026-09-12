//! The file tools (task M01-03): `read_file`, `write_file`, `edit_file`,
//! `list_dir` and `glob`.
//!
//! Every path the mentor names goes through [`Workspace::resolve`]; what
//! lands outside the root is refused as `denied` (M01-07 turns that into
//! a question for the user). Output formats are part of the contract:
//! the mentor reads them now, and the raw blobs are what the apprentice's
//! compressor (M03/M06) learns from later, so they are stable, plain and
//! machine-parseable — `<n>\t<line>` for file content, `name/` and
//! `name\t<bytes>` for listings, one path per line for `glob`, unified
//! diffs for changes, `[...]` trailers for truncation and notes.
//!
//! Writes are atomic (temp file in the same directory, fsync, rename)
//! and never rewrite line endings or BOMs on their own. Ignore rules
//! apply to discovery (`list_dir`, `glob`) only: `read_file` opens any
//! path the sandbox allows.

mod atomic;
mod diff;
mod edit;
mod glob;
mod list;
mod read;
#[cfg(test)]
mod tests;
mod text;
mod write;

use std::path::PathBuf;
use std::sync::Arc;

pub use edit::EditFile;
pub use glob::Glob;
pub use list::ListDir;
pub use read::ReadFile;
pub use write::WriteFile;

pub use atomic::write_atomic;
pub use diff::{Diff, unified_diff};

use super::{Tool, ToolContext, ToolError};
use crate::workspace::{PathError, Workspace};

/// `read_file` shows at most this many lines per call.
pub const READ_MAX_LINES: usize = 2000;
/// ... and at most this many bytes of content.
pub const READ_MAX_BYTES: usize = 200 * 1024;
/// `list_dir` prints at most this many entries.
pub const LIST_MAX_ENTRIES: usize = 2000;
/// Deepest `list_dir` tree.
pub const LIST_MAX_DEPTH: u64 = 4;
/// `glob` prints at most this many matches.
pub const GLOB_MAX_RESULTS: usize = 1000;

/// The five file tools, ready for [`super::ToolRegistry::register_all`].
pub fn file_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadFile),
        Arc::new(WriteFile),
        Arc::new(EditFile),
        Arc::new(ListDir),
        Arc::new(Glob),
    ]
}

/// A path the mentor named, resolved inside the workspace.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// Absolute, canonical as far as it exists.
    pub abs: PathBuf,
    /// The mentor-facing form (`display()`).
    pub shown: String,
}

/// Resolves `user_path` through the session's workspace.
///
/// # Errors
/// `Failed` without a workspace; `Denied` for a path outside it;
/// `InvalidInput` for a malformed one.
pub(crate) fn target(
    ctx: &ToolContext,
    user_path: &str,
) -> Result<(Arc<Workspace>, Target), ToolError> {
    let ws = ctx.workspace.clone().ok_or_else(|| {
        ToolError::Failed("this session has no workspace; the file tools need one".to_owned())
    })?;
    let abs = match ws.resolve(user_path) {
        Ok(abs) => abs,
        Err(PathError::Outside { path }) => {
            return Err(ToolError::Denied(format!(
                "`{path}` is outside the workspace `{}`; paths are relative to the workspace root",
                ws.display(ws.root())
            )));
        }
        Err(e @ PathError::Invalid { .. }) => return Err(ToolError::InvalidInput(e.to_string())),
        Err(PathError::Io { source, .. }) => return Err(ToolError::Io(source)),
    };
    let shown = ws.display(&abs);
    Ok((ws, Target { abs, shown }))
}

/// Parses the (already schema-validated) input.
pub(crate) fn parse<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, ToolError> {
    serde_json::from_value(value).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

/// Runs filesystem work on the blocking pool.
pub(crate) async fn blocking<T, F>(f: F) -> Result<T, ToolError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ToolError::Failed(format!("file task failed: {e}")))?
}

/// `+a −r` as shown in headers and summaries.
pub(crate) fn plus_minus(added: usize, removed: usize) -> String {
    format!("+{added} \u{2212}{removed}")
}

/// `"1 line"` / `"3 lines"`.
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}
