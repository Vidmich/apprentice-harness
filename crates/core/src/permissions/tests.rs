use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::events::{Event, Risk};
use apprentice_api::types::{
    PermissionAnswer, PermissionDecision, PermissionMode, PermissionSource, RuleEffect, RuleMatch,
    RuleSource, RuleSpec,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::rules::{
    Layer, append_rule, builtin_rules, command_has_prefix, evaluate, parse_rules, remove_rule,
};
use super::{
    Ask, Engine, NoClient, PermissionBroker, PermissionGate, PermissionRequest, Prompter, Reply,
    SessionRules, suggest,
};
use crate::config::{Headless, Paths, PermissionsConfig, ToolsConfig};
use crate::tools::{Executor, ToolCall, ToolRegistry, ToolResultKind, ToolSpec, builtin_tools};
use crate::trace::{NewAgent, NewSession, StepRef, TraceStore, TraceWriter, kinds};
use crate::workspace::Workspace;

// ------------------------------------------------------------- fixtures

fn spec(name: &str, risk: Risk, with_path: bool) -> ToolSpec {
    let schema = if with_path {
        json!({"type": "object", "properties": {"path": {"type": "string"}}})
    } else {
        json!({"type": "object", "properties": {"command": {"type": "string"}}})
    };
    ToolSpec::new(name, "", schema, risk)
}

/// A request without a workspace: every path is "outside".
fn req(tool: &str, risk: Risk, input: &Value) -> PermissionRequest {
    let with_path = input.get("path").is_some();
    PermissionRequest::for_call(&spec(tool, risk, with_path), input, None)
}

fn shell(command: &str) -> PermissionRequest {
    req("shell", Risk::Execute, &json!({"command": command}))
}

struct Fixture {
    _dir: tempfile::TempDir,
    ws: Arc<Workspace>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# x\n").unwrap();
        let ws = Arc::new(Workspace::open(dir.path()).unwrap());
        Self { _dir: dir, ws }
    }

    fn write(&self, path: &str) -> PermissionRequest {
        PermissionRequest::for_call(
            &spec("write_file", Risk::Write, true),
            &json!({"path": path, "content": "x"}),
            Some(&self.ws),
        )
    }

    fn read(&self, path: &str) -> PermissionRequest {
        PermissionRequest::for_call(
            &spec("read_file", Risk::ReadOnly, true),
            &json!({"path": path}),
            Some(&self.ws),
        )
    }

    fn shell(&self, command: &str) -> PermissionRequest {
        let spec = ToolSpec::new(
            "shell",
            "",
            json!({"type": "object", "properties": {"command": {}, "cwd": {}}}),
            Risk::Execute,
        );
        PermissionRequest::for_call(&spec, &json!({"command": command}), Some(&self.ws))
    }

    fn rules_file(&self) -> std::path::PathBuf {
        self.ws.permissions_file()
    }
}

fn layer(source: RuleSource, dir: &Path, text: &str) -> Layer {
    let path = dir.join(format!("{source:?}.toml"));
    std::fs::write(&path, text).unwrap();
    Layer::load(source, path)
}

/// `(tool, description, suggested rules, timeout_s)` of one prompt.
type Asked = (String, String, Vec<RuleSpec>, u64);

/// A prompter that answers from a script and records what it was asked.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    asked: Mutex<Vec<Asked>>,
}

impl Scripted {
    fn with(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Prompter for Scripted {
    async fn ask(&self, ask: &Ask<'_>) -> Reply {
        self.asked.lock().unwrap().push((
            ask.request.tool.clone(),
            ask.request.description.clone(),
            ask.suggested.to_vec(),
            ask.timeout.as_secs(),
        ));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted reply")
    }
}

fn answered(answer: PermissionAnswer) -> Reply {
    Reply::Answered { answer, rule: None }
}

fn agent() -> crate::trace::AgentId {
    crate::trace::AgentId::from("agent-1")
}

// ---------------------------------------------------------------- rules

#[test]
fn conditions_glob_prefix_regex_risk_and_outside() {
    let f = Fixture::new();
    let rule = |m: RuleMatch| {
        super::CompiledRule::compile(RuleSpec {
            tool: "*".into(),
            effect: RuleEffect::Allow,
            r#match: m,
        })
        .unwrap()
    };
    let path = |p: &str| {
        rule(RuleMatch {
            path: Some(p.into()),
            ..RuleMatch::default()
        })
    };
    assert!(path("src/**").matches(&f.write("src/a.rs")));
    assert!(path("src/**").matches(&f.write("src/deep/b.rs")));
    assert!(
        path("src/**").matches(&f.read("src")),
        "the directory itself"
    );
    assert!(!path("src/**").matches(&f.write("README.md")));
    assert!(
        !path("src/*").matches(&f.write("src/deep/b.rs")),
        "one level"
    );
    assert!(path("**").matches(&f.read(".")), "the root");
    assert!(path("**").matches(&f.write("src/a.rs")));
    assert!(!path("src/**").matches(&f.shell("ls")), "no path, no match");
    assert!(
        !path("**").matches(&f.read("../outside.txt")),
        "a glob reaches outside only with outside_workspace = true"
    );
    let out_glob = rule(RuleMatch {
        path: Some("**".into()),
        outside_workspace: Some(true),
        ..RuleMatch::default()
    });
    assert!(out_glob.matches(&f.read("../outside.txt")));
    assert!(!out_glob.matches(&f.read("src/a.rs")));

    let prefix = |p: &str| {
        rule(RuleMatch {
            command_prefix: Some(p.into()),
            ..RuleMatch::default()
        })
    };
    assert!(prefix("cargo test").matches(&shell("cargo test -p core")));
    assert!(prefix("cargo test").matches(&shell("  cargo test  ")));
    assert!(!prefix("cargo test").matches(&shell("cargo testx")));
    assert!(!prefix("cargo test").matches(&shell("cargo build")));
    assert!(
        !prefix("cargo test").matches(&shell("cargo test; rm -rf .")),
        "a chain is never a prefix match"
    );
    assert!(!prefix("cargo test").matches(&shell("cargo test && ls")));
    assert!(!prefix("cargo test").matches(&shell("cargo test $(ls)")));
    assert!(
        !prefix("cargo test").matches(&f.write("src/a.rs")),
        "no command"
    );
    assert!(command_has_prefix("ls", "ls"));
    assert!(!command_has_prefix("ls", ""));

    let regex = |r: &str| {
        rule(RuleMatch {
            command_regex: Some(r.into()),
            ..RuleMatch::default()
        })
    };
    assert!(regex(r"\bgit\s+push\b").matches(&shell("cd x && git push")));
    assert!(!regex(r"\bgit\s+push\b").matches(&shell("git pull")));

    let risk = rule(RuleMatch {
        risk: Some(Risk::ReadOnly),
        ..RuleMatch::default()
    });
    assert!(risk.matches(&f.read("src/a.rs")));
    assert!(!risk.matches(&f.write("src/a.rs")));

    let inside = rule(RuleMatch {
        outside_workspace: Some(false),
        ..RuleMatch::default()
    });
    let outside = rule(RuleMatch {
        outside_workspace: Some(true),
        ..RuleMatch::default()
    });
    assert!(inside.matches(&f.write("src/a.rs")));
    assert!(!outside.matches(&f.write("src/a.rs")));
    assert!(outside.matches(&f.write("../escape.txt")));
    assert!(!inside.matches(&f.write("../escape.txt")));
    assert!(inside.matches(&f.shell("ls")), "cwd defaults to the root");
    assert!(
        outside.matches(&req("read_file", Risk::ReadOnly, &json!({"path": "x"}))),
        "no workspace: every path is outside"
    );

    let named = super::CompiledRule::compile(RuleSpec {
        tool: "write_file".into(),
        effect: RuleEffect::Allow,
        r#match: RuleMatch::default(),
    })
    .unwrap();
    assert!(named.matches(&f.write("src/a.rs")));
    assert!(!named.matches(&f.read("src/a.rs")));
    assert_eq!(named.describe(), "write_file");
    assert_eq!(
        prefix("cargo test").describe(),
        "* [command_prefix=\"cargo test\"]"
    );
}

#[test]
fn bad_globs_and_regexes_are_refused() {
    let bad = |m: RuleMatch| {
        super::CompiledRule::compile(RuleSpec {
            tool: "shell".into(),
            effect: RuleEffect::Allow,
            r#match: m,
        })
        .unwrap_err()
    };
    assert!(
        bad(RuleMatch {
            command_regex: Some("(".into()),
            ..RuleMatch::default()
        })
        .starts_with("`match.command_regex`:")
    );
    assert!(
        bad(RuleMatch {
            path: Some("src/[".into()),
            ..RuleMatch::default()
        })
        .starts_with("`match.path`:")
    );
}

#[test]
fn precedence_across_layers_and_first_match_within() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let ws = layer(
        RuleSource::Workspace,
        dir.path(),
        r#"
[[rule]]
tool = "shell"
effect = "allow"
[rule.match]
command_prefix = "cargo test"

[[rule]]
tool = "shell"
effect = "ask"
[rule.match]
command_prefix = "cargo"

[[rule]]
tool = "write_file"
effect = "deny"
[rule.match]
path = "src/generated/**"
"#,
    );
    let user = layer(
        RuleSource::User,
        dir.path(),
        r#"
default = "deny"

[[rule]]
tool = "shell"
effect = "deny"
[rule.match]
command_prefix = "cargo test"

[[rule]]
tool = "shell"
effect = "allow"
[rule.match]
command_prefix = "cargo"

[[rule]]
tool = "write_file"
effect = "allow"
[rule.match]
path = "src/**"
"#,
    );
    let layers = [&ws, &user];
    let eval = |r: &PermissionRequest| {
        let e = evaluate(&layers, r);
        (e.effect, e.rule_ref.unwrap_or_default())
    };

    // user deny beats workspace allow.
    assert_eq!(
        eval(&f.shell("cargo test -q")),
        (RuleEffect::Deny, "user:1".into())
    );
    // workspace ask beats user allow; first match in the workspace file.
    assert_eq!(
        eval(&f.shell("cargo build")),
        (RuleEffect::Ask, "workspace:2".into())
    );
    // workspace deny beats user allow.
    assert_eq!(
        eval(&f.write("src/generated/x.rs")),
        (RuleEffect::Deny, "workspace:3".into())
    );
    // user allow when the workspace has no opinion.
    assert_eq!(
        eval(&f.write("src/a.rs")),
        (RuleEffect::Allow, "user:3".into())
    );
    // built-in allow for read-only inside, below both files.
    assert_eq!(
        eval(&f.read("README.md")),
        (RuleEffect::Allow, "builtin:read_only_inside".into())
    );
    // The explicit `default` of the first file that sets one (the
    // workspace file sets none).
    assert_eq!(
        eval(&f.write("README.md")),
        (RuleEffect::Deny, "default:user".into())
    );
    assert_eq!(
        eval(&f.read("../outside.txt")),
        (RuleEffect::Deny, "default:user".into())
    );

    // First match within a layer: an allow before a deny wins.
    let first = layer(
        RuleSource::User,
        dir.path(),
        r#"
[[rule]]
tool = "shell"
effect = "allow"
[rule.match]
command_prefix = "cargo"

[[rule]]
tool = "shell"
effect = "deny"
[rule.match]
command_prefix = "cargo publish"
"#,
    );
    let e = evaluate(&[&first], &f.shell("cargo publish"));
    assert_eq!(
        (e.effect, e.rule_ref.as_deref()),
        (RuleEffect::Allow, Some("user:1"))
    );

    // No files at all: read-only inside allowed, the rest asked.
    let missing = Layer::load(RuleSource::User, dir.path().join("missing.toml"));
    assert!(matches!(missing.state, super::rules::LayerState::Missing));
    let e = evaluate(&[&missing], &f.write("src/a.rs"));
    assert_eq!((e.effect, e.rule_ref), (RuleEffect::Ask, None));
    let e = evaluate(&[&missing], &f.read("src/a.rs"));
    assert_eq!(e.effect, RuleEffect::Allow);
}

#[test]
fn the_builtin_deny_list_catches_catastrophes_and_a_file_can_override() {
    let f = Fixture::new();
    let denied = |c: &str| {
        let e = evaluate(&[], &f.shell(c));
        assert_eq!(e.effect, RuleEffect::Deny, "{c}");
        e.rule_ref.unwrap()
    };
    assert_eq!(denied("rm -rf /"), "builtin:rm_recursive_root");
    assert_eq!(denied("sudo rm -rf /*"), "builtin:rm_recursive_root");
    assert_eq!(denied("rm -r ~"), "builtin:rm_recursive_root");
    assert_eq!(denied("cd /tmp && rm -fr ."), "builtin:rm_recursive_root");
    assert_eq!(denied("rm -rf .."), "builtin:rm_recursive_root");
    assert_eq!(denied("rm -rf C:\\"), "builtin:rm_recursive_root");
    assert_eq!(denied("rm -rf $HOME"), "builtin:rm_recursive_root");
    assert_eq!(
        denied("Remove-Item -Recurse -Force C:\\"),
        "builtin:remove_item_recursive_root"
    );
    assert_eq!(
        denied("Remove-Item / -Recurse"),
        "builtin:remove_item_recursive_root"
    );
    assert_eq!(denied("mkfs.ext4 /dev/sda1"), "builtin:disk_format");
    assert_eq!(denied("dd if=/dev/zero of=/dev/sda"), "builtin:disk_format");
    assert_eq!(denied("format c:"), "builtin:disk_format");
    assert_eq!(denied(":(){ :|:& };:"), "builtin:fork_bomb");
    assert_eq!(denied("chmod -R 777 /"), "builtin:chmod_recursive_root");

    let asked = |c: &str| {
        let e = evaluate(&[], &f.shell(c));
        assert_eq!(e.effect, RuleEffect::Ask, "{c}");
    };
    asked("rm -rf target");
    asked("rm -rf ./target/debug");
    asked("rm file.txt");
    asked("Remove-Item -Recurse target");
    asked("cargo test");
    asked("git rm -r src/old");
    asked("echo format c:");
    asked("chmod -R 755 ./scripts");

    // Any tool that carries the command is caught (task M01-15).
    let e = evaluate(
        &[],
        &req("run_tests", Risk::Execute, &json!({"command": "rm -rf /"})),
    );
    assert_eq!(e.effect, RuleEffect::Deny);
    assert_eq!(e.rule_ref.as_deref(), Some("builtin:rm_recursive_root"));
    let e = evaluate(&[], &req("run_tests", Risk::Execute, &json!({})));
    assert_eq!(e.effect, RuleEffect::Ask);

    // An explicit allow in a file wins over the built-in deny.
    let dir = tempfile::tempdir().unwrap();
    let user = layer(
        RuleSource::User,
        dir.path(),
        "[[rule]]\ntool = \"shell\"\neffect = \"allow\"\n[rule.match]\ncommand_regex = \"^rm -rf /tmp/scratch$\"\n",
    );
    let e = evaluate(&[&user], &f.shell("rm -rf /tmp/scratch"));
    assert_eq!(e.effect, RuleEffect::Allow);
    assert_eq!(builtin_rules().len(), 6);
}

#[test]
fn parse_errors_name_the_line_and_break_the_layer_into_ask() {
    let f = Fixture::new();
    let path = Path::new("permissions.toml");
    let err = parse_rules(path, "[[rule]]\ntool = \"shell\"\neffect = \"maybe\"\n").unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with("permissions.toml:3:"), "{text}");
    assert!(text.contains("maybe"), "{text}");

    let err = parse_rules(
        path,
        "[[rule]]\ntool = \"shell\"\neffect = \"allow\"\n\n[[rule]]\ntool = \"shell\"\neffect = \"deny\"\n[rule.match]\ncommand_regex = \"(\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().starts_with("permissions.toml:5:"), "{err}");

    let err = parse_rules(path, "default = \"allow\"\n").unwrap_err();
    assert!(err.to_string().contains("permissions.toml:1:"), "{err}");
    assert!(err.to_string().contains("\"ask\" or \"deny\""), "{err}");

    let err = parse_rules(
        path,
        "[[rule]]\ntool = \"shell\"\neffect = \"allow\"\n[rule.match]\ncmd = \"x\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("permissions.toml:5:"), "{err}");

    let (default, rules) = parse_rules(path, "").unwrap();
    assert_eq!(default, None);
    assert!(rules.is_empty());

    // A broken file forces ask even for what would be allowed, but
    // a deny in a sound file still holds.
    let dir = tempfile::tempdir().unwrap();
    let broken = layer(RuleSource::Workspace, dir.path(), "[[rule]]\ntool = 1\n");
    assert!(broken.error().is_some());
    let user = layer(
        RuleSource::User,
        dir.path(),
        "[[rule]]\ntool = \"shell\"\neffect = \"deny\"\n[rule.match]\ncommand_prefix = \"curl\"\n",
    );
    let e = evaluate(&[&broken, &user], &f.read("README.md"));
    assert_eq!(e.effect, RuleEffect::Ask);
    assert_eq!(e.broken.len(), 1);
    assert!(e.broken[0].contains("Workspace.toml:2:"), "{:?}", e.broken);
    let e = evaluate(&[&broken, &user], &f.shell("curl x"));
    assert_eq!(e.effect, RuleEffect::Deny);
    let info = broken.info();
    assert!(info.exists);
    assert!(info.error.unwrap().contains(":2:"));
}

#[test]
fn appended_rules_keep_the_file_sound_and_report_their_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/permissions.toml");
    let rule = RuleSpec {
        tool: "shell".into(),
        effect: RuleEffect::Allow,
        r#match: RuleMatch {
            command_prefix: Some("cargo test".into()),
            ..RuleMatch::default()
        },
    };
    let line = append_rule(&path, &rule, "first").unwrap();
    assert_eq!(line, 2);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "# first\n[[rule]]\ntool = \"shell\"\neffect = \"allow\"\n\n[rule.match]\ncommand_prefix = \"cargo test\"\n"
    );
    let deny = RuleSpec {
        tool: "write_file".into(),
        effect: RuleEffect::Deny,
        r#match: RuleMatch {
            path: Some("secrets/**".into()),
            outside_workspace: Some(false),
            ..RuleMatch::default()
        },
    };
    let line = append_rule(&path, &deny, "second\nline two").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let headers: Vec<u64> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| *l == "[[rule]]")
        .map(|(i, _)| i as u64 + 1)
        .collect();
    assert_eq!(headers, [2, line]);
    assert!(
        text.contains("\n\n# second\n# line two\n[[rule]]\n"),
        "{text}"
    );
    let (_, rules) = parse_rules(&path, &text).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[1].spec, deny);
    assert_eq!(rules[1].line, Some(line));

    // A hand-written file with a default and comments survives.
    let hand = dir.path().join("hand.toml");
    std::fs::write(&hand, "# mine\ndefault = \"deny\"\n").unwrap();
    append_rule(&hand, &rule, "").unwrap();
    let text = std::fs::read_to_string(&hand).unwrap();
    assert!(text.starts_with("# mine\ndefault = \"deny\"\n"), "{text}");
    let (default, rules) = parse_rules(&hand, &text).unwrap();
    assert_eq!(default, Some(apprentice_api::types::RuleDefault::Deny));
    assert_eq!(rules.len(), 1);

    // A bad rule is refused, a broken file is not appended to.
    let bad = RuleSpec {
        r#match: RuleMatch {
            command_regex: Some("(".into()),
            ..RuleMatch::default()
        },
        ..rule.clone()
    };
    assert!(append_rule(&path, &bad, "").is_err());
    let broken = dir.path().join("broken.toml");
    std::fs::write(&broken, "default = \n").unwrap();
    let err = append_rule(&broken, &rule, "").unwrap_err();
    assert!(err.to_string().contains("broken.toml:1:"), "{err}");
    assert_eq!(std::fs::read_to_string(&broken).unwrap(), "default = \n");
}

#[test]
fn removed_rules_leave_the_rest_of_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("permissions.toml");
    std::fs::write(&path, "# mine\ndefault = \"deny\"\n").unwrap();
    let allow = RuleSpec {
        tool: "shell".into(),
        effect: RuleEffect::Allow,
        r#match: RuleMatch {
            command_prefix: Some("cargo test".into()),
            ..RuleMatch::default()
        },
    };
    let deny = RuleSpec {
        tool: "write_file".into(),
        effect: RuleEffect::Deny,
        r#match: RuleMatch {
            path: Some("secrets/**".into()),
            ..RuleMatch::default()
        },
    };
    append_rule(&path, &allow, "first").unwrap();
    append_rule(&path, &deny, "second").unwrap();

    // Out of range, and the file is untouched.
    let before = std::fs::read_to_string(&path).unwrap();
    let err = remove_rule(&path, 3).unwrap_err();
    assert!(err.to_string().contains("no rule 3"), "{err}");
    assert!(remove_rule(&path, 0).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    assert_eq!(remove_rule(&path, 1).unwrap(), allow);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# mine\ndefault = \"deny\"\n"), "{text}");
    assert!(!text.contains("cargo test"), "{text}");
    assert!(text.contains("# second\n[[rule]]\n"), "{text}");
    let (default, rules) = parse_rules(&path, &text).unwrap();
    assert_eq!(default, Some(apprentice_api::types::RuleDefault::Deny));
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].spec, deny);

    // The last one goes with its `rule` array; the header stays.
    assert_eq!(remove_rule(&path, 1).unwrap(), deny);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("[[rule]]"), "{text}");
    let (default, rules) = parse_rules(&path, &text).unwrap();
    assert_eq!(default, Some(apprentice_api::types::RuleDefault::Deny));
    assert!(rules.is_empty());
    assert!(remove_rule(&dir.path().join("none.toml"), 1).is_err());
}

#[test]
fn requests_and_suggestions() {
    let f = Fixture::new();
    let w = f.write("src\\a.rs");
    assert_eq!(w.shown_paths(), ["src/a.rs"]);
    assert!(!w.has_outside());
    assert_eq!(w.description, "write_file: src/a.rs");
    assert_eq!(
        suggest(&w)
            .iter()
            .map(|r| r.r#match.path.clone().unwrap())
            .collect::<Vec<_>>(),
        ["src/**", "src/a.rs"]
    );
    let root = f.write("README.md");
    assert_eq!(
        suggest(&root)
            .iter()
            .map(|r| r.r#match.path.clone().unwrap())
            .collect::<Vec<_>>(),
        ["**", "README.md"]
    );
    let dir = f.read(".");
    assert_eq!(suggest(&dir).len(), 1);
    assert_eq!(suggest(&dir)[0].r#match.path.as_deref(), Some("**"));

    let out = f.write("../escape.txt");
    assert!(out.has_outside());
    let s = suggest(&out);
    assert_eq!(s[0].r#match.outside_workspace, Some(true));
    assert!(s[0].r#match.path.as_deref().unwrap().ends_with("/**"));

    let c = f.shell("cargo test -p core --lib");
    assert_eq!(c.command.as_deref(), Some("cargo test -p core --lib"));
    assert_eq!(c.shown_paths(), ["."], "cwd defaults to the root");
    assert_eq!(c.description, "shell: cargo test -p core --lib");
    let s = suggest(&c);
    assert_eq!(
        s.iter()
            .map(|r| r.r#match.command_prefix.clone().unwrap())
            .collect::<Vec<_>>(),
        ["cargo test", "cargo"]
    );
    assert!(
        s.iter()
            .all(|r| r.tool == "shell" && r.effect == RuleEffect::Allow)
    );
    let chain = f.shell("cargo build && cargo test");
    let s = suggest(&chain);
    assert_eq!(s.len(), 1);
    assert_eq!(
        s[0].r#match.command_regex.as_deref(),
        Some(r"^cargo build \&\& cargo test$")
    );
    let one = shell("ls");
    assert_eq!(suggest(&one).len(), 1);

    let described = PermissionRequest::for_call(
        &spec("shell", Risk::Execute, false),
        &json!({"command": "ls", "description": "  list files "}),
        Some(&f.ws),
    );
    assert_eq!(described.description, "list files");
    let bare = req("shell_jobs", Risk::ReadOnly, &json!({"action": "list"}));
    assert!(bare.paths.is_empty());
    assert_eq!(bare.description, "shell_jobs");
    assert_eq!(suggest(&bare)[0].r#match, RuleMatch::default());

    // The view keeps paths and commands whole and cuts long content.
    let long = PermissionRequest::for_call(
        &spec("write_file", Risk::Write, true),
        &json!({"path": "x".repeat(3000), "content": "y".repeat(3000)}),
        None,
    );
    let view = long.input_view();
    assert_eq!(view["path"].as_str().unwrap().len(), 3000);
    let content = view["content"].as_str().unwrap();
    assert!(
        content.ends_with("… [952 more chars]"),
        "{}",
        &content[2040..]
    );
}

// --------------------------------------------------------------- engine

fn engine(f: &Fixture, dir: &Path, config: PermissionsConfig) -> Arc<Engine> {
    Arc::new(Engine::with_files(
        dir.join("user-permissions.toml"),
        Some(f.rules_file()),
        config,
        Arc::new(SessionRules::new()),
    ))
}

fn short_timeout() -> PermissionsConfig {
    PermissionsConfig {
        ask_timeout_s: 1,
        ..PermissionsConfig::default()
    }
}

#[tokio::test]
async fn answers_decide_and_write_rules() {
    let f = Fixture::new();
    let home = tempfile::tempdir().unwrap();
    let engine = engine(&f, home.path(), short_timeout());
    let prompter = Scripted::with([
        answered(PermissionAnswer::AllowOnce),
        answered(PermissionAnswer::DenyOnce),
        answered(PermissionAnswer::AllowSession),
        answered(PermissionAnswer::AllowWorkspace),
        answered(PermissionAnswer::DenyAlways),
        Reply::Answered {
            answer: PermissionAnswer::AllowAlways,
            rule: Some(RuleSpec {
                tool: "shell".into(),
                effect: RuleEffect::Ask,
                r#match: RuleMatch {
                    command_prefix: Some("cargo".into()),
                    ..RuleMatch::default()
                },
            }),
        },
    ]);
    let mode = PermissionMode::Default;
    let decide = |r: PermissionRequest| {
        let engine = Arc::clone(&engine);
        let prompter = Arc::clone(&prompter);
        async move { engine.decide(&r, &agent(), mode, &*prompter).await }
    };

    // Read-only inside: allowed by the built-in, nobody asked.
    let o = decide(f.read("src/a.rs")).await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::Rule);
    assert_eq!(o.rule_ref.as_deref(), Some("builtin:read_only_inside"));
    assert!(o.request_id.is_none());
    assert!(prompter.asked().is_empty());

    // allow_once.
    let o = decide(f.write("src/a.rs")).await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::User);
    assert_eq!(o.answer, Some(PermissionAnswer::AllowOnce));
    assert!(o.request_id.is_some());
    let asked = prompter.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, "write_file: src/a.rs");
    assert_eq!(asked[0].3, 1);

    // deny_once.
    let o = decide(f.write("src/a.rs")).await;
    assert!(!o.is_allowed());
    assert_eq!(o.reason.as_deref(), Some("denied by user"));

    // allow_session: the suggested rule (src/**) is remembered.
    let o = decide(f.write("src/a.rs")).await;
    assert!(o.is_allowed());
    assert_eq!(o.rule_ref.as_deref(), Some("session:1"));
    assert_eq!(engine.session_rules().len(), 1);
    let o = decide(f.write("src/other.rs")).await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::Session);
    assert_eq!(o.rule_ref.as_deref(), Some("session:1"));
    assert_eq!(prompter.asked().len(), 3, "not asked again");

    // allow_workspace writes `.harness/permissions.toml`; the next
    // identical call matches it without asking.
    let o = decide(f.write("README.md")).await;
    assert!(o.is_allowed());
    let written = o.rule_written.expect("rule written");
    assert_eq!(written.path, f.rules_file());
    assert_eq!(written.rule.r#match.path.as_deref(), Some("**"));
    let text = std::fs::read_to_string(f.rules_file()).unwrap();
    assert!(
        text.contains("# allow_workspace for write_file: README.md"),
        "{text}"
    );
    assert!(text.contains("path = \"**\""), "{text}");
    let o = decide(f.write("README.md")).await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::Rule);
    assert_eq!(o.rule_ref.as_deref(), Some("workspace:1"));
    assert_eq!(prompter.asked().len(), 4);

    // deny_always writes a deny rule to the user file.
    let o = decide(f.shell("curl https://example.com")).await;
    assert!(!o.is_allowed());
    let written = o.rule_written.expect("rule written");
    assert_eq!(written.path, home.path().join("user-permissions.toml"));
    assert_eq!(written.rule.effect, RuleEffect::Deny);
    assert_eq!(
        written.rule.r#match.command_prefix.as_deref(),
        Some("curl https://example.com")
    );
    let o = decide(f.shell("curl https://example.com --fail")).await;
    assert!(!o.is_allowed());
    assert_eq!(o.source, PermissionSource::Rule);
    assert_eq!(o.rule_ref.as_deref(), Some("user:1"));
    assert!(
        o.reason
            .as_deref()
            .unwrap()
            .starts_with("denied by rule user:1 (shell [command_prefix="),
        "{:?}",
        o.reason
    );

    // allow_always with an edited rule: the answer's effect wins over
    // the rule's.
    let o = decide(f.shell("cargo build")).await;
    assert!(o.is_allowed());
    let written = o.rule_written.expect("rule written");
    assert_eq!(written.rule.effect, RuleEffect::Allow);
    assert_eq!(
        written.rule.r#match.command_prefix.as_deref(),
        Some("cargo")
    );
    let o = decide(f.shell("cargo doc")).await;
    assert!(o.is_allowed());
    assert_eq!(o.rule_ref.as_deref(), Some("user:2"));
    assert_eq!(prompter.asked().len(), 6);
}

#[tokio::test]
async fn timeout_headless_and_modes() {
    let f = Fixture::new();
    let home = tempfile::tempdir().unwrap();

    // Timeout → deny.
    let engine_ = engine(&f, home.path(), short_timeout());
    let prompter = Scripted::with([Reply::TimedOut]);
    let o = engine_
        .decide(
            &f.write("src/a.rs"),
            &agent(),
            PermissionMode::Default,
            &*prompter,
        )
        .await;
    assert!(!o.is_allowed());
    assert_eq!(o.source, PermissionSource::Timeout);
    assert!(
        o.reason.as_deref().unwrap().contains("within 1 s"),
        "{:?}",
        o.reason
    );

    // Headless deny (the default).
    let o = engine_
        .decide(
            &f.write("src/a.rs"),
            &agent(),
            PermissionMode::Default,
            &NoClient,
        )
        .await;
    assert!(!o.is_allowed());
    assert_eq!(o.source, PermissionSource::Headless);
    assert!(o.reason.as_deref().unwrap().contains("no client attached"));

    // Headless allow_readonly: reads outside the workspace pass, writes
    // do not.
    let lenient = engine(
        &f,
        home.path(),
        PermissionsConfig {
            headless: Headless::AllowReadonly,
            ..short_timeout()
        },
    );
    let o = lenient
        .decide(
            &f.read("../outside.txt"),
            &agent(),
            PermissionMode::Default,
            &NoClient,
        )
        .await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::Headless);
    let o = lenient
        .decide(
            &f.write("src/a.rs"),
            &agent(),
            PermissionMode::Default,
            &NoClient,
        )
        .await;
    assert!(!o.is_allowed());
    assert_eq!(o.source, PermissionSource::Headless);
    assert!(o.reason.as_deref().unwrap().contains("read-only"));

    // plan: writes and commands denied before any rule, reads run.
    let prompter = Scripted::with([]);
    let o = engine_
        .decide(
            &f.write("src/a.rs"),
            &agent(),
            PermissionMode::Plan,
            &*prompter,
        )
        .await;
    assert!(!o.is_allowed());
    assert_eq!(o.source, PermissionSource::Mode);
    assert_eq!(o.rule_ref.as_deref(), Some("mode:plan"));
    assert_eq!(
        o.reason.as_deref(),
        Some("denied: plan mode is read-only and write_file is a write tool")
    );
    let o = engine_
        .decide(
            &f.shell("cargo test"),
            &agent(),
            PermissionMode::Plan,
            &*prompter,
        )
        .await;
    assert!(!o.is_allowed());
    let o = engine_
        .decide(
            &f.read("src/a.rs"),
            &agent(),
            PermissionMode::Plan,
            &*prompter,
        )
        .await;
    assert!(o.is_allowed());
    assert!(prompter.asked().is_empty());

    // auto: writes inside allowed without asking; execute and outside
    // still asked.
    let prompter = Scripted::with([
        answered(PermissionAnswer::DenyOnce),
        answered(PermissionAnswer::DenyOnce),
    ]);
    let o = engine_
        .decide(
            &f.write("src/a.rs"),
            &agent(),
            PermissionMode::Auto,
            &*prompter,
        )
        .await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::Mode);
    assert_eq!(o.rule_ref.as_deref(), Some("mode:auto"));
    let o = engine_
        .decide(
            &f.shell("cargo test"),
            &agent(),
            PermissionMode::Auto,
            &*prompter,
        )
        .await;
    assert!(!o.is_allowed());
    let o = engine_
        .decide(
            &f.write("../escape.txt"),
            &agent(),
            PermissionMode::Auto,
            &*prompter,
        )
        .await;
    assert!(!o.is_allowed());
    assert_eq!(prompter.asked().len(), 2);

    // A deny rule holds in auto mode too.
    std::fs::create_dir_all(f.rules_file().parent().unwrap()).unwrap();
    std::fs::write(
        f.rules_file(),
        "[[rule]]\ntool = \"write_file\"\neffect = \"deny\"\n[rule.match]\npath = \"src/**\"\n",
    )
    .unwrap();
    let o = engine_
        .decide(
            &f.write("src/a.rs"),
            &agent(),
            PermissionMode::Auto,
            &*prompter,
        )
        .await;
    assert!(!o.is_allowed());
    assert_eq!(o.rule_ref.as_deref(), Some("workspace:1"));

    // A broken workspace file: asked (and noted), never allowed.
    std::fs::write(f.rules_file(), "[[rule]]\ntool = \"write_file\"\n").unwrap();
    let prompter = Scripted::with([answered(PermissionAnswer::AllowOnce)]);
    let o = engine_
        .decide(
            &f.read("src/a.rs"),
            &agent(),
            PermissionMode::Default,
            &*prompter,
        )
        .await;
    assert!(o.is_allowed());
    assert_eq!(o.source, PermissionSource::User);
    assert_eq!(o.notes.len(), 1);
    assert!(o.notes[0].contains("permissions.toml:1:"), "{:?}", o.notes);
    let (rules, files) = engine_.listing();
    assert_eq!(files.len(), 2);
    assert!(files[0].error.is_some());
    assert!(!files[1].exists);
    assert_eq!(rules.len(), 6, "built-ins only");
}

// ----------------------------------------------------------------- gate

struct Trace {
    _home: tempfile::TempDir,
    store: Arc<TraceStore>,
    writer: TraceWriter,
    at: StepRef,
}

impl Trace {
    fn new(ws: &Workspace) -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let store = Arc::new(TraceStore::open(&paths).unwrap());
        let session = store
            .create_session(&NewSession {
                title: None,
                workspace_path: Some(ws.root_string()),
                workspace_id: None,
                config: json!({}),
            })
            .unwrap();
        let agent = store
            .start_agent(&NewAgent::main(session.clone(), "task"))
            .unwrap();
        let step = store.start_step(&agent).unwrap();
        let writer = TraceWriter::spawn(Arc::clone(&store));
        Self {
            _home: home,
            store,
            writer,
            at: StepRef {
                session,
                agent,
                step,
            },
        }
    }

    fn decisions(&self) -> Vec<Value> {
        self.store
            .session_events(&self.at.session)
            .unwrap()
            .into_iter()
            .filter(|e| e.summary.kind == kinds::PERMISSION_DECISION)
            .map(|e| e.payload)
            .collect()
    }
}

#[tokio::test]
async fn the_gate_records_every_decision_and_the_executor_obeys() {
    let f = Fixture::new();
    let home = tempfile::tempdir().unwrap();
    let store = Trace::new(&f.ws);
    let registry = ToolRegistry::new();
    registry.register_all(builtin_tools()).unwrap();
    let config = ToolsConfig::default();
    let engine = engine(&f, home.path(), short_timeout());
    let prompter = Scripted::with([
        answered(PermissionAnswer::DenyOnce),
        answered(PermissionAnswer::AllowOnce),
    ]);
    let (events, mut live) = broadcast::channel::<Event>(16);
    let gate = PermissionGate::new(
        Arc::clone(&engine),
        prompter.clone(),
        &store.writer,
        store.at.clone(),
        PermissionMode::Default,
    )
    .with_sink(Arc::new(events));
    let executor = Executor::new(
        &registry,
        &gate,
        &store.writer,
        &config,
        store.at.clone(),
        CancellationToken::new(),
    )
    .with_workspace(Some(Arc::clone(&f.ws)));

    let read = executor
        .execute_one(ToolCall::new(
            "c1",
            "read_file",
            json!({"path": "src/a.rs"}),
        ))
        .await;
    assert_eq!(read.kind, ToolResultKind::Ok, "{read:?}");
    let denied = executor
        .execute_one(ToolCall::new(
            "c2",
            "write_file",
            json!({"path": "src/a.rs", "content": "changed\n"}),
        ))
        .await;
    assert_eq!(denied.kind, ToolResultKind::Denied, "{denied:?}");
    assert_eq!(denied.summary, "write_file: denied");
    assert_eq!(
        std::fs::read_to_string(f.ws.root().join("src/a.rs")).unwrap(),
        "fn a() {}\n",
        "a denied write changes nothing"
    );
    let allowed = executor
        .execute_one(ToolCall::new(
            "c3",
            "write_file",
            json!({"path": "src/a.rs", "content": "changed\n"}),
        ))
        .await;
    assert_eq!(allowed.kind, ToolResultKind::Ok, "{allowed:?}");
    assert_eq!(
        std::fs::read_to_string(f.ws.root().join("src/a.rs")).unwrap(),
        "changed\n"
    );
    store.writer.flush().await.unwrap();

    let decisions = store.decisions();
    assert_eq!(decisions.len(), 3, "{decisions:#?}");
    assert_eq!(decisions[0]["call_id"], "c1");
    assert_eq!(decisions[0]["decision"], "allow");
    assert_eq!(decisions[0]["source"], "rule");
    assert_eq!(decisions[0]["rule_ref"], "builtin:read_only_inside");
    assert_eq!(decisions[0]["asked"], false);
    assert_eq!(decisions[0]["paths"], json!(["src/a.rs"]));
    assert_eq!(decisions[0]["mode"], "default");
    assert_eq!(decisions[1]["call_id"], "c2");
    assert_eq!(decisions[1]["decision"], "deny");
    assert_eq!(decisions[1]["source"], "user");
    assert_eq!(decisions[1]["answer"], "deny_once");
    assert_eq!(decisions[1]["reason"], "denied by user");
    assert_eq!(decisions[1]["asked"], true);
    assert!(decisions[1]["request_id"].is_string());
    assert_eq!(decisions[2]["decision"], "allow");
    assert_eq!(decisions[2]["answer"], "allow_once");

    // The mentor reads the reason in the tool result.
    let block = serde_json::to_value(&denied.block).unwrap();
    assert_eq!(block["is_error"], true);
    assert_eq!(block["content"][0]["text"], "denied by user");

    // Live decision events, one per call.
    let mut seen = Vec::new();
    while let Ok(ev) = live.try_recv() {
        if let Event::PermissionDecision {
            call_id, decision, ..
        } = ev
        {
            seen.push((call_id, decision));
        }
    }
    assert_eq!(
        seen,
        [
            ("c1".to_owned(), PermissionDecision::Allow),
            ("c2".to_owned(), PermissionDecision::Deny),
            ("c3".to_owned(), PermissionDecision::Allow),
        ]
    );
    store.writer.shutdown().await;
}

#[tokio::test]
async fn cancelling_the_agent_ends_a_pending_prompt() {
    let f = Fixture::new();
    let home = tempfile::tempdir().unwrap();
    let store = Trace::new(&f.ws);
    let registry = ToolRegistry::new();
    registry.register_all(builtin_tools()).unwrap();
    let config = ToolsConfig::default();
    let engine = engine(
        &f,
        home.path(),
        PermissionsConfig {
            ask_timeout_s: 600,
            ..PermissionsConfig::default()
        },
    );
    let broker = Arc::new(PermissionBroker::new());
    let (events, mut live) = broadcast::channel::<Event>(16);
    let prompter = Arc::new(super::AgentPrompter::new(
        Arc::clone(&broker),
        Arc::new(events.clone()),
    ));
    let gate = PermissionGate::new(
        Arc::clone(&engine),
        prompter,
        &store.writer,
        store.at.clone(),
        PermissionMode::Default,
    );
    let cancel = CancellationToken::new();
    let executor = Executor::new(
        &registry,
        &gate,
        &store.writer,
        &config,
        store.at.clone(),
        cancel.clone(),
    )
    .with_workspace(Some(Arc::clone(&f.ws)));

    let call = executor.execute_one(ToolCall::new(
        "c1",
        "write_file",
        json!({"path": "src/a.rs", "content": "x"}),
    ));
    let waiter = async {
        let ev = tokio::time::timeout(Duration::from_secs(5), live.recv())
            .await
            .unwrap()
            .unwrap();
        let Event::PermissionRequest { request_id, .. } = ev else {
            panic!("expected a request, got {ev:?}");
        };
        assert_eq!(broker.pending().len(), 1);
        cancel.cancel();
        request_id
    };
    let (executed, request_id) = tokio::join!(call, waiter);
    assert_eq!(executed.kind, ToolResultKind::Cancelled, "{executed:?}");
    assert!(broker.pending().is_empty(), "the request is forgotten");
    let err = broker
        .respond(&request_id, PermissionAnswer::AllowOnce, None)
        .unwrap_err();
    assert_eq!(err.kind(), Some("not_found"));
    store.writer.shutdown().await;
}

#[tokio::test]
async fn the_broker_settles_a_prompt_with_the_first_answer() {
    let f = Fixture::new();
    let broker = Arc::new(PermissionBroker::new());
    let (events, mut live) = broadcast::channel::<Event>(16);
    let prompter = super::AgentPrompter::new(Arc::clone(&broker), Arc::new(events.clone()));
    let request = f.shell("cargo test");
    let suggested = suggest(&request);
    let ask = Ask {
        request_id: "req-1",
        agent_id: &agent(),
        request: &request,
        suggested: &suggested,
        timeout: Duration::from_secs(5),
    };
    let answer = async {
        let ev = live.recv().await.unwrap();
        let Event::PermissionRequest {
            request_id,
            tool,
            command,
            paths,
            suggested_rules,
            timeout_s,
            description,
            ..
        } = ev
        else {
            panic!("{ev:?}");
        };
        assert_eq!(request_id, "req-1");
        assert_eq!(tool, "shell");
        assert_eq!(command.as_deref(), Some("cargo test"));
        assert_eq!(paths, ["."]);
        assert_eq!(suggested_rules, suggested);
        assert_eq!(timeout_s, 5);
        assert_eq!(description, "shell: cargo test");
        assert_eq!(
            broker.pending(),
            [("req-1".to_owned(), "agent-1".to_owned())]
        );
        broker
            .respond("req-1", PermissionAnswer::AllowSession, None)
            .unwrap();
        // The second answer is too late.
        let err = broker
            .respond("req-1", PermissionAnswer::DenyOnce, None)
            .unwrap_err();
        assert_eq!(err.kind(), Some("not_found"));
    };
    let (reply, ()) = tokio::join!(prompter.ask(&ask), answer);
    assert_eq!(
        reply,
        Reply::Answered {
            answer: PermissionAnswer::AllowSession,
            rule: None
        }
    );

    // No subscriber: headless at once.
    drop(live);
    let reply = prompter.ask(&ask).await;
    assert_eq!(reply, Reply::NoClient);
    assert!(broker.pending().is_empty());

    // A subscriber that never answers: timeout.
    let _live = events.subscribe();
    let ask = Ask {
        timeout: Duration::from_millis(50),
        ..ask
    };
    let reply = prompter.ask(&ask).await;
    assert_eq!(reply, Reply::TimedOut);
    assert!(broker.pending().is_empty());

    // Session rules are per session.
    let a = broker.session_rules(&crate::trace::SessionId::from("s1"));
    a.add(suggested[0].clone()).unwrap();
    assert_eq!(
        broker
            .session_rules(&crate::trace::SessionId::from("s1"))
            .len(),
        1
    );
    assert_eq!(
        broker
            .session_rules(&crate::trace::SessionId::from("s2"))
            .len(),
        0
    );
    broker.forget_session(&crate::trace::SessionId::from("s1"));
    assert_eq!(
        broker
            .session_rules(&crate::trace::SessionId::from("s1"))
            .len(),
        0
    );
}
