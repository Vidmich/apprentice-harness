//! Output conventions (task M00-09): human text on stdout, one JSON
//! document with `--json`, informational messages on stderr, colours only
//! on a terminal with `NO_COLOR` unset, and one error format.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};

use apprentice_api::jsonrpc::RpcError;
use apprentice_client::{ClientError, ConnectError};
use owo_colors::OwoColorize as _;
use serde::Serialize;

/// Set once a `--json` document went to stdout, so an error afterwards
/// does not add a second one.
static JSON_EMITTED: AtomicBool = AtomicBool::new(false);

/// Where and how a command prints.
#[derive(Debug, Clone, Copy)]
pub struct Out {
    /// `--json`: machine-readable stdout, human text to stderr.
    pub json: bool,
    /// `--quiet`: no informational messages on stderr.
    pub quiet: bool,
    color: bool,
}

impl Out {
    pub fn new(json: bool, quiet: bool) -> Self {
        Self {
            json,
            quiet,
            color: !json && color_wanted(),
        }
    }

    /// Prints the document `--json` callers get.
    ///
    /// # Errors
    /// Serialisation failure (a bug in the type).
    #[allow(clippy::unused_self)] // one call shape for every output kind
    pub fn emit_json<T: Serialize + ?Sized>(self, value: &T) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string_pretty(value)?);
        JSON_EMITTED.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// One NDJSON line (streaming commands).
    #[allow(clippy::unused_self)]
    pub fn emit_json_line<T: Serialize + ?Sized>(self, value: &T) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string(value)?);
        JSON_EMITTED.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// One line of human output on stdout.
    #[allow(clippy::unused_self)]
    pub fn line(self, text: impl AsRef<str>) {
        println!("{}", text.as_ref());
    }

    /// An informational message: stderr, dropped with `--quiet`.
    pub fn info(self, text: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("{}", text.as_ref());
        }
    }

    /// `key  value` lines, keys padded to the widest one.
    pub fn print_kv<K: AsRef<str>, V: AsRef<str>>(self, pairs: &[(K, V)]) {
        print!("{}", self.format_kv(pairs));
    }

    pub fn format_kv<K: AsRef<str>, V: AsRef<str>>(self, pairs: &[(K, V)]) -> String {
        let width = pairs
            .iter()
            .map(|(k, _)| k.as_ref().chars().count())
            .max()
            .unwrap_or(0);
        let mut s = String::new();
        for (k, v) in pairs {
            let key = format!("{:<width$}", k.as_ref());
            let _ = writeln!(s, "{}  {}", self.dim(&key), v.as_ref());
        }
        s
    }

    /// A left-aligned table with a header row; empty `rows` print
    /// `(none)`.
    pub fn print_table<S: AsRef<str>>(self, header: &[&str], rows: &[Vec<S>]) {
        print!("{}", self.format_table(header, rows));
    }

    pub fn format_table<S: AsRef<str>>(self, header: &[&str], rows: &[Vec<S>]) -> String {
        if rows.is_empty() {
            return format!("{}\n", self.dim("(none)"));
        }
        let widths: Vec<usize> = (0..header.len())
            .map(|i| {
                rows.iter()
                    .filter_map(|r| r.get(i))
                    .map(|c| c.as_ref().chars().count())
                    .chain(std::iter::once(header[i].chars().count()))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let line = |cells: &[&str]| -> String {
            let mut s = String::new();
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    s.push_str("  ");
                }
                let _ = write!(s, "{cell:<w$}", w = widths[i]);
            }
            s.trim_end().to_owned()
        };
        let mut out = String::new();
        let _ = writeln!(out, "{}", self.dim(&line(header)));
        for r in rows {
            let cells: Vec<&str> = r.iter().map(AsRef::as_ref).collect();
            out.push_str(&line(&cells));
            out.push('\n');
        }
        out
    }

    pub fn bold(self, s: &str) -> String {
        if self.color {
            s.bold().to_string()
        } else {
            s.to_owned()
        }
    }

    pub fn dim(self, s: &str) -> String {
        if self.color {
            s.dimmed().to_string()
        } else {
            s.to_owned()
        }
    }

    pub fn red(self, s: &str) -> String {
        if self.color {
            s.red().to_string()
        } else {
            s.to_owned()
        }
    }

    /// Reports a failed command: `error: <message>` on stderr, plus an
    /// `{"error": ...}` document on stdout with `--json` unless the command
    /// already printed its document (a `run` result carries the error).
    pub fn report_error(self, e: &anyhow::Error) {
        eprintln!("{} {}", self.red("error:"), describe(e));
        if self.json && !JSON_EMITTED.load(Ordering::Relaxed) {
            let rpc = e.chain().find_map(rpc_of);
            let doc = serde_json::json!({
                "error": {
                    "message": describe(e),
                    "kind": rpc.and_then(RpcError::kind),
                    "code": rpc.map(|r| r.code),
                    "details": rpc.and_then(|r| r.data.as_ref()).and_then(|d| d.details.clone()),
                }
            });
            println!("{doc}");
        }
    }
}

/// Colour when stdout is a terminal and `NO_COLOR` is unset (`FORCE_COLOR`
/// wins, for tests).
fn color_wanted() -> bool {
    if std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::io::stdout().is_terminal()
}

/// The message chain joined with `: `, without the duplicates that
/// transparent wrappers produce, and RPC errors shown as
/// `message [kind] (details)`.
pub fn describe(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        let text = match rpc_of(cause) {
            Some(rpc) => describe_rpc(rpc),
            None => cause.to_string(),
        };
        if text.is_empty() || out.contains(&text) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&text);
    }
    out
}

/// The RPC error behind `cause`, through the client's transparent wrappers.
fn rpc_of<'a>(cause: &'a (dyn std::error::Error + 'static)) -> Option<&'a RpcError> {
    if let Some(rpc) = cause.downcast_ref::<RpcError>() {
        return Some(rpc);
    }
    if let Some(ClientError::Rpc(rpc)) = cause.downcast_ref::<ClientError>() {
        return Some(rpc);
    }
    if let Some(ConnectError::Client(ClientError::Rpc(rpc))) = cause.downcast_ref::<ConnectError>()
    {
        return Some(rpc);
    }
    None
}

/// `mentor rejected the request [mentor_error] (401 authentication_error)`.
pub fn describe_rpc(e: &RpcError) -> String {
    let mut s = e.message.clone();
    if let Some(kind) = e.kind().filter(|k| !k.is_empty()) {
        let _ = write!(s, " [{kind}]");
    }
    if let Some(details) = e.data.as_ref().and_then(|d| d.details.as_ref()) {
        let text = match details {
            serde_json::Value::String(t) => t.clone(),
            serde_json::Value::Object(map) => {
                let known: Vec<String> = ["status", "type", "reason"]
                    .iter()
                    .filter_map(|k| map.get(*k))
                    .map(|v| match v {
                        serde_json::Value::String(t) => t.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                if known.is_empty() {
                    details.to_string()
                } else {
                    known.join(" ")
                }
            }
            other => other.to_string(),
        };
        if !text.is_empty() && text.chars().count() <= 200 {
            let _ = write!(s, " ({text})");
        }
    }
    s
}

/// Flushes stdout; streaming output relies on it.
pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// Flushes stderr, for a prompt without a newline.
pub fn flush_stderr() {
    let _ = std::io::stderr().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprentice_api::jsonrpc::RpcError;

    fn plain() -> Out {
        Out {
            json: false,
            quiet: false,
            color: false,
        }
    }

    #[test]
    fn kv_and_table_align() {
        let out = plain();
        assert_eq!(
            out.format_kv(&[("pid", "42"), ("endpoint", "pipe:x")]),
            "pid       42\nendpoint  pipe:x\n"
        );
        assert_eq!(
            out.format_table(&["id", "title"], &[vec!["1", "alpha"], vec!["22", ""]]),
            "id  title\n1   alpha\n22\n"
        );
        assert_eq!(out.format_table::<&str>(&["id"], &[]), "(none)\n");
    }

    #[test]
    fn rpc_errors_show_kind_and_details() {
        let e = RpcError::new(-32020, "mentor_error", "mentor rejected the request")
            .with_details(serde_json::json!({"status": 401, "type": "authentication_error"}));
        assert_eq!(
            describe_rpc(&e),
            "mentor rejected the request [mentor_error] (401 authentication_error)"
        );
        let e = RpcError::not_found("no such session");
        assert_eq!(describe_rpc(&e), "no such session [not_found]");

        // Transparent wrappers do not repeat the message.
        let wrapped: anyhow::Error =
            apprentice_client::ConnectError::Client(apprentice_client::ClientError::Rpc(e)).into();
        assert_eq!(describe(&wrapped), "no such session [not_found]");
        let with_context = wrapped.context("listing sessions");
        assert_eq!(
            describe(&with_context),
            "listing sessions: no such session [not_found]"
        );
    }
}
