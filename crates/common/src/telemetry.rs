//! Structured logging for every process (daemon, CLI, GUI backend).
//!
//! Two `tracing` layers: a compact human format on stderr and daily-rotating
//! JSON lines under `<log_dir>/<role>.<date>.log` (14 files kept). Both go
//! through a [`RedactingWriter`] that blanks the values of fields named
//! `api_key`, `token` and `authorization` (or ending in `_api_key`,
//! `_token`, `_authorization`), so a key can never leak through a careless
//! `debug!(api_key = ...)` even in third-party code.

use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Environment variable that sets the log level for all processes.
pub const LEVEL_ENV: &str = "HARNESS_LOG_LEVEL";

/// Number of daily log files kept per role.
pub const MAX_LOG_FILES: usize = 14;

/// Field names whose values are redacted (matched as whole names or as
/// `_`-separated suffixes, case-insensitively).
pub const REDACTED_FIELDS: &[&str] = &["api_key", "token", "authorization"];

/// How to initialise logging for one process.
#[derive(Debug, Clone)]
pub struct Options {
    /// `daemon`, `cli` or `gui`; names the log file.
    pub role: &'static str,
    /// Filter directive, e.g. `info` or `apprentice_core=debug,info`.
    pub level: String,
    /// `<data_dir>/logs`; created if missing.
    pub log_dir: PathBuf,
    /// Also write the compact format to stderr.
    pub stderr: bool,
    /// ANSI colours on stderr; `None` = only when stderr is a terminal.
    pub ansi: Option<bool>,
}

impl Options {
    pub fn new(role: &'static str, level: impl Into<String>, log_dir: impl Into<PathBuf>) -> Self {
        Self {
            role,
            level: level.into(),
            log_dir: log_dir.into(),
            stderr: true,
            ansi: None,
        }
    }

    #[must_use]
    pub fn stderr(mut self, on: bool) -> Self {
        self.stderr = on;
        self
    }
}

/// What `init` set up; keep it for the life of the process.
#[derive(Debug, Clone)]
pub struct Handle {
    pub log_dir: PathBuf,
    /// Today's log file for this role.
    pub log_file: PathBuf,
    /// The filter that was applied (level plus `RUST_LOG` directives).
    pub filter: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("cannot create log directory {path}: {source}")]
    LogDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot open log file in {path}: {message}")]
    LogFile { path: PathBuf, message: String },
    #[error("invalid log filter `{filter}`: {message}")]
    Filter { filter: String, message: String },
    #[error("logging is already initialised")]
    AlreadyInitialized,
}

/// Picks the effective level: flag > `HARNESS_LOG_LEVEL` > config > `info`.
/// Each argument is the value from that source, if any.
pub fn resolve_level(flag: Option<&str>, env: Option<&str>, config: Option<&str>) -> String {
    [flag, env, config]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("info")
        .to_owned()
}

/// `HARNESS_LOG_LEVEL` from the process environment, if set and non-empty.
pub fn level_from_env() -> Option<String> {
    std::env::var(LEVEL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Builds the filter string: the level, plus `RUST_LOG` directives for
/// per-target control.
pub fn filter_string(level: &str) -> String {
    match std::env::var("RUST_LOG") {
        Ok(rust_log) if !rust_log.trim().is_empty() => format!("{level},{rust_log}"),
        _ => level.to_owned(),
    }
}

/// Installs the global subscriber and the panic hook. Call once per process.
///
/// # Errors
/// Log directory or file cannot be created, the filter is malformed, or a
/// subscriber is already installed.
pub fn init(options: &Options) -> Result<Handle, TelemetryError> {
    std::fs::create_dir_all(&options.log_dir).map_err(|source| TelemetryError::LogDir {
        path: options.log_dir.clone(),
        source,
    })?;
    let filter = filter_string(&options.level);
    let env_filter = EnvFilter::try_new(&filter).map_err(|e| TelemetryError::Filter {
        filter: filter.clone(),
        message: e.to_string(),
    })?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(options.role)
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(&options.log_dir)
        .map_err(|e| TelemetryError::LogFile {
            path: options.log_dir.clone(),
            message: e.to_string(),
        })?;
    let log_file = current_log_file(&options.log_dir, options.role);

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_target(true)
        .with_writer(Redacting(appender));

    let ansi = options
        .ansi
        .unwrap_or_else(|| io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none());
    let stderr_layer = options.stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(ansi)
            .with_target(false)
            .with_writer(Redacting(io::stderr))
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .with(stderr_layer.map(Layer::boxed))
        .try_init()
        .map_err(|_| TelemetryError::AlreadyInitialized)?;

    install_panic_hook();
    tracing::debug!(role = options.role, filter, "logging initialised");
    Ok(Handle {
        log_dir: options.log_dir.clone(),
        log_file,
        filter,
    })
}

/// Path of today's log file for a role (what the rolling appender writes).
pub fn current_log_file(log_dir: &Path, role: &str) -> PathBuf {
    let today = time_today();
    log_dir.join(format!("{role}.{today}.log"))
}

fn time_today() -> String {
    // `tracing_appender` names files by UTC date; mirror that without
    // pulling a date crate in: civil-from-days (Howard Hinnant).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Logs panics at `error` with a backtrace before the default hook runs.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let message = panic_message(info);
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        tracing::error!(
            target: "panic",
            %location,
            thread = std::thread::current().name().unwrap_or("?"),
            backtrace,
            "panic: {message}"
        );
        previous(info);
    }));
}

/// Human message of a panic payload.
pub fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

// ----------------------------------------------------------- redaction

/// Blanks secret-looking field values in one formatted log line. Handles
/// the JSON form (`"api_key":"v"`) and the compact form (`api_key=v` /
/// `api_key="v"`).
pub fn redact_line(line: &str) -> Cow<'_, str> {
    let lower = line.to_ascii_lowercase();
    let mut edits: Vec<(usize, usize)> = Vec::new(); // value byte ranges
    for name in REDACTED_FIELDS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(name) {
            let start = from + pos;
            let end = start + name.len();
            from = end;
            if !is_key_boundary_before(line, start) {
                continue;
            }
            if let Some(range) = value_range_after(line, end) {
                edits.push(range);
            }
        }
    }
    if edits.is_empty() {
        return Cow::Borrowed(line);
    }
    edits.sort_unstable();
    edits.dedup();
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    for (s, e) in edits {
        if s < cursor {
            continue; // overlapping (nested) match already handled
        }
        out.push_str(&line[cursor..s]);
        out.push_str("***");
        cursor = e;
    }
    out.push_str(&line[cursor..]);
    Cow::Owned(out)
}

fn is_key_boundary_before(line: &str, start: usize) -> bool {
    match line[..start].chars().next_back() {
        None => true,
        Some(c) => c == '"' || c == '_' || c == '.' || c == ' ' || c == '{' || c == ',' || c == '(',
    }
}

/// If a key ends at `end`, returns the byte range of its value.
fn value_range_after(line: &str, end: usize) -> Option<(usize, usize)> {
    let rest = &line[end..];
    let bytes = rest.as_bytes();
    let mut i = 0;
    // JSON key: `"` then optional spaces then `:`; compact: optional `"`? no —
    // compact keys are bare, then `=`.
    if bytes.first() == Some(&b'"') {
        i += 1;
        while bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
    } else if bytes.first() == Some(&b'=') {
        i += 1;
    } else {
        return None;
    }
    while bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    let value_start = end + i;
    if bytes.get(i) == Some(&b'"') {
        // Quoted: up to the next unescaped quote.
        let mut j = i + 1;
        while let Some(&b) = bytes.get(j) {
            match b {
                b'\\' => j += 2,
                b'"' => return Some((value_start + 1, end + j)),
                _ => j += 1,
            }
        }
        Some((value_start + 1, line.len()))
    } else {
        let mut j = i;
        while let Some(&b) = bytes.get(j) {
            if matches!(b, b' ' | b',' | b'}' | b'\n' | b'\r') {
                break;
            }
            j += 1;
        }
        if j == i {
            return None;
        }
        Some((value_start, end + j))
    }
}

/// Line-buffering writer that redacts each complete line.
#[derive(Debug)]
pub struct RedactingWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::new(),
        }
    }

    fn flush_line(&mut self, line: &[u8]) -> io::Result<()> {
        match std::str::from_utf8(line) {
            Ok(s) => self.inner.write_all(redact_line(s).as_bytes()),
            Err(_) => self.inner.write_all(line),
        }
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            self.flush_line(&line)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.flush_line(&line)?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// `MakeWriter` adapter wrapping every writer in a [`RedactingWriter`].
#[derive(Debug, Clone)]
pub struct Redacting<M>(pub M);

impl<'a, M> MakeWriter<'a> for Redacting<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(self.0.make_writer())
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        RedactingWriter::new(self.0.make_writer_for(meta))
    }
}

/// `tracing::Level` for a plain level word, if it is one.
pub fn parse_level(s: &str) -> Option<Level> {
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_precedence() {
        assert_eq!(
            resolve_level(Some("trace"), Some("debug"), Some("warn")),
            "trace"
        );
        assert_eq!(resolve_level(None, Some("debug"), Some("warn")), "debug");
        assert_eq!(resolve_level(None, Some(" "), Some("warn")), "warn");
        assert_eq!(resolve_level(None, None, None), "info");
    }

    #[test]
    fn redacts_json_and_compact_forms() {
        let json = r#"{"fields":{"api_key":"sk-ant-123","input_tokens":12,"daemon_token":"abc","user":"x"}}"#;
        assert_eq!(
            redact_line(json),
            r#"{"fields":{"api_key":"***","input_tokens":12,"daemon_token":"***","user":"x"}}"#
        );
        let compact = r#"2026 DEBUG hello api_key="sk-ant-123" token=abc123 tokens=5 authorization="Bearer x""#;
        assert_eq!(
            redact_line(compact),
            r#"2026 DEBUG hello api_key="***" token=*** tokens=5 authorization="***""#
        );
        // Escaped quotes inside the value and mixed case names.
        assert_eq!(
            redact_line(r#"{"Authorization":"a\"b","x":1}"#),
            r#"{"Authorization":"***","x":1}"#
        );
        // Not a key: plain words and non-matching suffixes are untouched.
        let plain = "the token count is 5; tokenizer=fast; api_keys=3";
        assert_eq!(redact_line(plain), plain);
    }

    #[test]
    fn writer_redacts_per_line_and_flushes_partial() {
        let mut out = Vec::new();
        {
            let mut w = RedactingWriter::new(&mut out);
            w.write_all(b"a api_key=1\nb tok").unwrap();
            w.write_all(b"en=2\npartial authorization=3").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a api_key=***\nb token=***\npartial authorization=***"
        );
    }

    #[test]
    fn today_is_iso_date() {
        let d = time_today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }
}
