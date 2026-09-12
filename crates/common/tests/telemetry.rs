//! End-to-end logging: JSON file output with redaction, pruning of old
//! files, single initialisation, and the panic hook.

use std::io::Write;
use std::sync::{Arc, Mutex};

use apprentice_common::telemetry::{self, MAX_LOG_FILES, Options, TelemetryError};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// The one test that installs the global subscriber.
#[test]
fn init_writes_redacted_json_lines_prunes_old_files_and_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    for day in 1..=20 {
        std::fs::write(
            log_dir.join(format!("daemon.2020-01-{day:02}.log")),
            "old\n",
        )
        .unwrap();
    }
    // Files of another role are not touched by pruning.
    std::fs::write(log_dir.join("cli.2020-01-01.log"), "old\n").unwrap();

    let handle = telemetry::init(&Options::new("daemon", "debug", &log_dir).stderr(false)).unwrap();
    assert_eq!(handle.log_dir, log_dir);
    assert!(handle.filter.starts_with("debug"));

    let span = tracing::info_span!("rpc", method = "auth.set_key", conn = 7u64);
    let guard = span.enter();
    tracing::info!(
        api_key = "sk-ant-SECRET",
        request_token = "tok-SECRET",
        n = 3,
        "hello"
    );
    tracing::debug!("visible at debug");
    tracing::trace!("hidden at debug");
    drop(guard);

    let text = std::fs::read_to_string(&handle.log_file).unwrap();
    assert!(!text.contains("SECRET"), "{text}");
    let hello = text
        .lines()
        .find(|l| l.contains("hello"))
        .expect("hello line");
    let v: serde_json::Value = serde_json::from_str(hello).unwrap();
    assert_eq!(v["level"], "INFO");
    assert_eq!(v["fields"]["message"], "hello");
    assert_eq!(v["fields"]["api_key"], "***");
    assert_eq!(v["fields"]["request_token"], "***");
    assert_eq!(v["fields"]["n"], 3);
    assert_eq!(v["span"]["method"], "auth.set_key");
    assert!(v["timestamp"].is_string() && v["target"].is_string());
    assert!(text.contains("visible at debug"));
    assert!(!text.contains("hidden at debug"));

    let daemon_files: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("daemon."))
        .collect();
    assert!(
        daemon_files.len() <= MAX_LOG_FILES,
        "{} files kept: {daemon_files:?}",
        daemon_files.len()
    );
    assert!(daemon_files.iter().any(|n| n.ends_with(&format!(
        "{}",
        handle.log_file.file_name().unwrap().to_string_lossy()
    ))));
    assert!(log_dir.join("cli.2020-01-01.log").exists());

    assert!(matches!(
        telemetry::init(&Options::new("daemon", "info", &log_dir)),
        Err(TelemetryError::AlreadyInitialized)
    ));
}

#[test]
fn panic_hook_logs_message_location_and_backtrace() {
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        telemetry::install_panic_hook();
        let r = std::panic::catch_unwind(|| panic!("boom {}", 42));
        assert!(r.is_err());
    });
    let text = buf.text();
    assert!(text.contains("ERROR"), "{text}");
    assert!(text.contains("panic: boom 42"), "{text}");
    assert!(
        text.contains("location=") && text.contains("telemetry.rs"),
        "{text}"
    );
    assert!(text.contains("backtrace="), "{text}");
}
