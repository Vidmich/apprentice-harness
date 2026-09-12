//! Layered configuration and secrets (task M00-03), run against a temp
//! `HARNESS_HOME`-style directory. No process environment is touched: env
//! overrides are injected through `ConfigLoader::with_env`.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::jsonrpc::codes;
use apprentice_api::methods::{
    AuthSetKeyParams, AuthStatus, AuthStatusResult, ConfigGet, ConfigGetParams, ConfigPath,
    ConfigPathResult, ConfigSet, ConfigSetParams, Empty, Method,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::{ConfigLayer, ConfigSource, Effort};
use apprentice_client::{ClientOptions, DaemonClient};
use apprentice_core::config::{self, Config, ConfigError, ConfigLoader, ConfigService, Paths};
use apprentice_core::secrets::{ChainStore, EnvStore, FileStore, Secret, SecretStore};
use serde_json::json;
use tempfile::TempDir;

struct Home {
    _dir: TempDir,
    paths: Paths,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(dir.path());
    Home { _dir: dir, paths }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn defaults_load_with_no_files() {
    let h = home();
    let r = ConfigLoader::new(h.paths.clone()).load(None).unwrap();
    assert_eq!(r.config, Config::builtin());
    assert_eq!(r.config.mentor.model, "claude-opus-5");
    assert_eq!(r.config.mentor.effort, Effort::High);
    assert!(r.user_file.is_none() && r.workspace_file.is_none());
    assert_eq!(r.source("mentor.model"), Some(ConfigSource::Default));
    assert!(r.sources.values().all(|s| *s == ConfigSource::Default));
    let (v, src) = r.get("pricing.claude-opus-5.input").unwrap();
    assert_eq!((v, src), (json!(5.0), Some(ConfigSource::Default)));
    let (table, src) = r.get("mentor").unwrap();
    assert!(table.is_object() && src.is_none());
}

#[test]
fn layers_merge_with_precedence_and_provenance() {
    let h = home();
    let ws = tempfile::tempdir().unwrap();
    write(
        &h.paths.config_file(),
        "[mentor]\nmodel = \"claude-sonnet-5\"\nmax_tokens = 1000\n\n[daemon]\nlog_level = \"debug\"\n",
    );
    write(
        &Paths::workspace_config_file(ws.path()),
        "[mentor]\nmodel = \"claude-haiku-4-5-20251001\"\n[apprentice]\nenabled = true\n",
    );
    let loader = ConfigLoader::new(h.paths.clone()).with_env([("HARNESS_MENTOR_EFFORT", "low")]);

    let r = loader.load(Some(ws.path())).unwrap();
    assert_eq!(r.config.mentor.model, "claude-haiku-4-5-20251001");
    assert_eq!(r.source("mentor.model"), Some(ConfigSource::Workspace));
    assert_eq!(r.config.mentor.max_tokens, 1000);
    assert_eq!(r.source("mentor.max_tokens"), Some(ConfigSource::User));
    assert_eq!(r.config.daemon.log_level, "debug");
    assert!(r.config.apprentice.enabled);
    assert_eq!(r.config.mentor.effort, Effort::Low);
    assert_eq!(r.source("mentor.effort"), Some(ConfigSource::Env));
    assert_eq!(r.source("mentor.base_url"), Some(ConfigSource::Default));
    assert!(r.user_file.is_some() && r.workspace_file.is_some());

    // Without the workspace, the user layer wins.
    let r = loader.load(None).unwrap();
    assert_eq!(r.config.mentor.model, "claude-sonnet-5");
    assert!(!r.config.apprentice.enabled);
}

#[test]
fn env_override_beats_workspace_and_user() {
    let h = home();
    let ws = tempfile::tempdir().unwrap();
    write(&h.paths.config_file(), "[mentor]\nmodel = \"u\"\n");
    write(
        &Paths::workspace_config_file(ws.path()),
        "[mentor]\nmodel = \"w\"\n",
    );
    let r = ConfigLoader::new(h.paths.clone())
        .with_env([
            ("HARNESS_MENTOR_MODEL", "e"),
            ("HARNESS_LOG_LEVEL", "trace"),
        ])
        .load(Some(ws.path()))
        .unwrap();
    assert_eq!(r.config.mentor.model, "e");
    assert_eq!(r.config.daemon.log_level, "trace");
    assert_eq!(r.source("daemon.log_level"), Some(ConfigSource::Env));
}

#[test]
fn invalid_env_value_names_the_variable() {
    let h = home();
    let err = ConfigLoader::new(h.paths.clone())
        .with_env([("HARNESS_MENTOR_EFFORT", "ultra")])
        .load(None)
        .unwrap_err();
    match err {
        ConfigError::Invalid { path, message } => {
            assert_eq!(path.to_str().unwrap(), "$HARNESS_MENTOR_EFFORT");
            assert!(message.contains("ultra"), "{message}");
        }
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn workspace_cannot_override_protected_keys() {
    let h = home();
    let ws = tempfile::tempdir().unwrap();
    let file = Paths::workspace_config_file(ws.path());
    write(&file, "[mentor]\nbase_url = \"http://evil\"\n");
    let err = ConfigLoader::new(h.paths.clone())
        .load(Some(ws.path()))
        .unwrap_err();
    match &err {
        ConfigError::KeyNotOverridable { path, key, allowed } => {
            assert_eq!(path, &file);
            assert_eq!(key, "mentor.base_url");
            assert!(allowed.contains("mentor.model"));
        }
        other => panic!("unexpected {other}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains("mentor.base_url") && msg.contains(".harness"),
        "{msg}"
    );
}

#[test]
fn unknown_key_wrong_type_and_syntax_errors_name_file_and_key() {
    let h = home();
    let file = h.paths.config_file();

    write(&file, "[mentor]\nmodle = \"x\"\n");
    let err = ConfigLoader::new(h.paths.clone()).load(None).unwrap_err();
    assert!(
        matches!(&err, ConfigError::UnknownKey { path, key } if path == &file && key == "mentor.modle"),
        "{err}"
    );

    write(&file, "[mentor]\nmax_tokens = \"lots\"\n");
    let err = ConfigLoader::new(h.paths.clone()).load(None).unwrap_err();
    assert!(
        matches!(&err, ConfigError::WrongType { key, expected: "number", found: "string", .. } if key == "mentor.max_tokens"),
        "{err}"
    );

    write(&file, "mentor = 5\n");
    let err = ConfigLoader::new(h.paths.clone()).load(None).unwrap_err();
    assert!(
        matches!(&err, ConfigError::WrongType { key, expected: "table", .. } if key == "mentor"),
        "{err}"
    );

    write(&file, "[mentor\n");
    let err = ConfigLoader::new(h.paths.clone()).load(None).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Syntax { path, .. } if path == &file),
        "{err}"
    );

    // Unknown pricing field is rejected; a new model is accepted.
    write(&file, "[pricing.\"my-model\"]\ninput = 1.0\n");
    let err = ConfigLoader::new(h.paths.clone()).load(None).unwrap_err();
    assert!(matches!(&err, ConfigError::Invalid { .. }), "{err}"); // missing fields
    write(
        &file,
        "[pricing.\"my-model\"]\ninput = 1\noutput = 2\ncache_read = 0.1\ncache_write = 1.25\n",
    );
    let r = ConfigLoader::new(h.paths.clone()).load(None).unwrap();
    assert!((r.config.pricing["my-model"].input - 1.0).abs() < f64::EPSILON);
    write(&file, "[pricing.\"my-model\"]\ninputs = 1.0\n");
    let err = ConfigLoader::new(h.paths.clone()).load(None).unwrap_err();
    assert!(
        matches!(&err, ConfigError::UnknownKey { key, .. } if key == "pricing.my-model.inputs"),
        "{err}"
    );
}

#[test]
fn set_preserves_comments_and_creates_files() {
    let h = home();
    let file = h.paths.config_file();
    write(
        &file,
        "# my config\n[mentor]\nmodel = \"claude-sonnet-5\" # fast\n\n# trace settings\n[trace]\ncapture_raw_sse = true\n",
    );
    config::set(
        &h.paths,
        ConfigLayer::User,
        None,
        "mentor.effort",
        json!("max"),
    )
    .unwrap();
    config::set(
        &h.paths,
        ConfigLayer::User,
        None,
        "mentor.model",
        json!("claude-opus-5"),
    )
    .unwrap();
    config::set(
        &h.paths,
        ConfigLayer::User,
        None,
        "pricing.\"my-model\"",
        json!({"input": 3, "output": 6}),
    )
    .unwrap();
    config::set(
        &h.paths,
        ConfigLayer::User,
        None,
        "pricing.\"my-model\".output",
        json!(7.5),
    )
    .unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# my config\n"), "{text}");
    assert!(text.contains("model = \"claude-opus-5\" # fast"), "{text}");
    assert!(text.contains("# trace settings"), "{text}");
    assert!(text.contains("effort = \"max\""), "{text}");
    assert!(
        text.contains(
            "[pricing.my-model]
input = 3
output = 7.5
"
        ),
        "{text}"
    );
    assert!(
        !text.contains("[pricing]\n"),
        "no empty parent header: {text}"
    );
    let r = ConfigLoader::new(h.paths.clone()).load(None).unwrap();
    assert_eq!(r.config.mentor.effort, Effort::Max);

    // Removing a key falls back to the default.
    config::set(
        &h.paths,
        ConfigLayer::User,
        None,
        "mentor.effort",
        json!(null),
    )
    .unwrap();
    let r = ConfigLoader::new(h.paths.clone()).load(None).unwrap();
    assert_eq!(r.config.mentor.effort, Effort::High);
    assert_eq!(r.source("mentor.effort"), Some(ConfigSource::Default));

    // Workspace file is created on demand, and the whitelist applies.
    let ws = tempfile::tempdir().unwrap();
    config::set(
        &h.paths,
        ConfigLayer::Workspace,
        Some(ws.path()),
        "apprentice.enabled",
        json!(true),
    )
    .unwrap();
    assert!(Paths::workspace_config_file(ws.path()).is_file());
    let err = config::set(
        &h.paths,
        ConfigLayer::Workspace,
        Some(ws.path()),
        "daemon.log_level",
        json!("x"),
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::KeyNotOverridable { .. }),
        "{err}"
    );
    assert!(matches!(
        config::set(
            &h.paths,
            ConfigLayer::Workspace,
            None,
            "apprentice.enabled",
            json!(true)
        ),
        Err(ConfigError::WorkspaceRequired)
    ));

    // Bad writes never touch the file.
    let before = std::fs::read_to_string(&file).unwrap();
    for (key, value) in [
        ("mentor.nope", json!(1)),
        ("mentor.max_tokens", json!("many")),
        ("mentor", json!(5)),
        ("mentor", json!({"modle": "x"})),
        ("mentor.effort", json!("ultra")),
        ("bad..key", json!(1)),
    ] {
        let err = config::set(&h.paths, ConfigLayer::User, None, key, value).unwrap_err();
        assert!(!matches!(err, ConfigError::Write { .. }), "{key}: {err}");
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
}

#[test]
fn secrets_env_beats_file_and_status_reports_source() {
    let h = home();
    let file: Arc<dyn SecretStore> = Arc::new(FileStore::new(h.paths.secrets_file()));
    let chain = ChainStore::new(
        Arc::new(EnvStore::from_map([("ANTHROPIC_API_KEY", "env-key")])),
        file.clone(),
    );
    chain
        .set("anthropic_api_key", &Secret::new("file-key"))
        .unwrap();
    let (v, src) = chain.lookup("anthropic_api_key").unwrap().unwrap();
    assert_eq!((v.expose(), src), ("env-key", "env"));

    let no_env: Arc<dyn SecretStore> = Arc::new(EnvStore::from_map::<[(&str, &str); 0], _, _>([]));
    let chain = ChainStore::new(no_env, file);
    let (v, src) = chain.lookup("anthropic_api_key").unwrap().unwrap();
    assert_eq!((v.expose(), src), ("file-key", "file"));
}

/// The acceptance test for "never in logs": a captured `tracing` log of a
/// struct holding a `Secret` contains no key material.
#[test]
fn secret_never_appears_in_captured_logs() {
    use std::io::Write;
    use std::sync::Mutex;
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

    #[derive(Debug)]
    #[allow(dead_code)]
    struct Creds {
        provider: &'static str,
        key: Secret,
    }

    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let creds = Creds {
            provider: "anthropic",
            key: Secret::new("sk-ant-SUPERSECRET"),
        };
        tracing::info!(?creds, "configured");
        tracing::warn!(key = ?creds.key, "again");
    });
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("configured") && log.contains("anthropic"),
        "{log}"
    );
    assert!(!log.contains("SUPERSECRET"), "{log}");
    assert!(log.contains("***"), "{log}");
}

#[tokio::test]
#[allow(clippy::many_single_char_names)]
async fn rpc_handlers_over_a_router() {
    let h = home();
    let secrets = ChainStore::new(
        Arc::new(EnvStore::from_map::<[(&str, &str); 0], _, _>([])),
        Arc::new(FileStore::new(h.paths.secrets_file())),
    );
    let svc = Arc::new(ConfigService::new(
        ConfigLoader::new(h.paths.clone()),
        secrets,
    ));
    let mut router = Router::new(RouterConfig {
        daemon_version: "0".into(),
        pid: 1,
        token: None,
    });
    svc.register(&mut router);
    assert_eq!(
        router.methods(),
        vec![
            "auth.set_key",
            "auth.status",
            "config.get",
            "config.path",
            "config.set"
        ]
    );

    let (server_side, client_side) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_side);
    let router = Arc::new(router);
    tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
    client.hello("test", "0", None).await.unwrap();

    let p: ConfigPathResult = client.call::<ConfigPath>(Empty {}).await.unwrap();
    assert_eq!(Path::new(&p.config_file), h.paths.config_file());

    let r = client
        .call::<ConfigGet>(ConfigGetParams {
            key: Some("mentor.model".into()),
            workspace: None,
        })
        .await
        .unwrap();
    assert_eq!(r.value, json!("claude-opus-5"));
    assert_eq!(r.source, Some(ConfigSource::Default));

    client
        .call::<ConfigSet>(ConfigSetParams {
            key: "mentor.model".into(),
            value: json!("claude-sonnet-5"),
            layer: ConfigLayer::User,
            workspace: None,
        })
        .await
        .unwrap();
    let r = client
        .call::<ConfigGet>(ConfigGetParams {
            key: Some("mentor.model".into()),
            workspace: None,
        })
        .await
        .unwrap();
    assert_eq!(r.value, json!("claude-sonnet-5"));
    assert_eq!(r.source, Some(ConfigSource::User));

    let whole = client
        .call::<ConfigGet>(ConfigGetParams::default())
        .await
        .unwrap();
    assert_eq!(whole.value["mentor"]["model"], "claude-sonnet-5");
    assert!(whole.source.is_none());

    // Errors carry the config code and a machine-readable reason.
    let err = client
        .call::<ConfigSet>(ConfigSetParams {
            key: "mentor.nope".into(),
            value: json!(1),
            layer: ConfigLayer::User,
            workspace: None,
        })
        .await
        .unwrap_err();
    let apprentice_client::ClientError::Rpc(e) = err else {
        panic!("expected rpc error");
    };
    assert_eq!(e.code, codes::CONFIG_ERROR);
    let details = e.data.unwrap().details.unwrap();
    assert_eq!(details["reason"], "unknown_key");
    assert_eq!(details["key"], "mentor.nope");
    let err = client
        .call::<ConfigGet>(ConfigGetParams {
            key: Some("mentor.nope".into()),
            workspace: None,
        })
        .await
        .unwrap_err();
    let apprentice_client::ClientError::Rpc(e) = err else {
        panic!("expected rpc error");
    };
    assert_eq!(e.code, codes::NOT_FOUND);

    // auth.*
    let s: AuthStatusResult = client.call::<AuthStatus>(Empty {}).await.unwrap();
    assert_eq!(s.providers.len(), 1);
    assert!(!s.providers[0].configured && s.providers[0].source.is_none());

    let err = client
        .call::<apprentice_api::methods::AuthSetKey>(AuthSetKeyParams {
            provider: "openai".into(),
            key: "x".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, apprentice_client::ClientError::Rpc(e) if e.code == codes::INVALID_PARAMS)
    );

    client
        .call::<apprentice_api::methods::AuthSetKey>(AuthSetKeyParams {
            provider: "anthropic".into(),
            key: "  sk-ant-test  ".into(),
        })
        .await
        .unwrap();
    let s: AuthStatusResult = client.call::<AuthStatus>(Empty {}).await.unwrap();
    assert!(s.providers[0].configured);
    assert_eq!(s.providers[0].source.as_deref(), Some("file"));
    // The status never carries the key; the file holds the trimmed value.
    assert!(!serde_json::to_string(&s).unwrap().contains("sk-ant"));
    let stored = std::fs::read_to_string(h.paths.secrets_file()).unwrap();
    assert!(
        stored.contains("anthropic_api_key = \"sk-ant-test\""),
        "{stored}"
    );
    let _ = AuthStatus::NAME;
}
