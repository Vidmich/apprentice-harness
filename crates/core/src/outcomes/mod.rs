//! Outcome signals (task M01-15): the machine-readable end of a
//! trajectory, recorded as `outcome {kind, details}` events so a later
//! milestone can label runs without reading transcripts (SPEC §9, §10).
//!
//! | kind | producer | details |
//! |---|---|---|
//! | `files_changed` | the runtime, from the start and end snapshots | `changed`, `added`, `deleted`, `counts` |
//! | `tests` | the `run_tests` tool, and the runtime's heuristics on `shell` runs | `runner`, `passed`, `failed`, `skipped`, `exit_code`, `duration_ms`, `command`, `source`, `parsed` |
//! | `build` | heuristics on `shell` runs (`cargo build`, `tsc`, `pnpm build`, ...) | `ok`, `exit_code`, `duration_ms`, `command` |
//! | `user_accept` / `user_reject` / `task_done` | `session.mark` | `note?` |
//! | `error` | the runtime | `kind` (`refusal`, `max_iterations`, `context_limit`, `stalled`, `daemon_restart`, ...) |
//! | `reverted` | the runtime, at the next run's start snapshot | `files`, `at_agent` |
//!
//! [`runners`] knows the test runners: which one a workspace uses,
//! which one a command invokes, and how to read each one's summary.
//! [`Outcome`] is one signal before it is recorded; [`describe`] turns a
//! recorded payload back into the one line clients show, so events
//! written before this module get a summary too. Labels are noisy by
//! nature (a test run may pass for reasons unrelated to the task); they
//! are recorded faithfully and interpreted later (M04).

pub mod runners;

use std::time::Duration;

use apprentice_api::events::Event;
use apprentice_api::types::{OutcomeInfo, SessionMark};
use serde_json::{Value, json};

pub use runners::{CommandKind, Detected, Runner, TestCounts, classify_command, detect_runner};

use crate::trace::{AgentId, NewEvent, SessionId, StepRef, kinds};

/// Outcome kinds, as `outcome.kind`.
pub mod kind {
    pub const FILES_CHANGED: &str = "files_changed";
    pub const TESTS: &str = "tests";
    pub const BUILD: &str = "build";
    pub const USER_ACCEPT: &str = "user_accept";
    pub const USER_REJECT: &str = "user_reject";
    pub const TASK_DONE: &str = "task_done";
    pub const ERROR: &str = "error";
    pub const REVERTED: &str = "reverted";

    /// The kinds that label a run for training: a verdict on the
    /// result, or a test run.
    pub const LABELS: &[&str] = &[TESTS, USER_ACCEPT, USER_REJECT, TASK_DONE];
}

/// One outcome signal, before it is recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub kind: &'static str,
    pub details: Value,
}

/// What a test or build run left, as the tools and the heuristics
/// report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFacts {
    pub command: String,
    pub exit_code: Option<i32>,
    pub duration: Duration,
    /// Whether the run was killed (timeout, cancellation): no verdict.
    pub killed: bool,
}

impl Outcome {
    /// A `tests` outcome from a run's output. `runner` is what the
    /// command was recognised as (or told to be); without one, or when
    /// the output carries no summary the parser knows, the outcome is
    /// exit-code-only (`parsed: false`).
    pub fn tests(runner: Option<Runner>, output: &str, facts: &RunFacts, source: &str) -> Self {
        let counts = runner.map(|r| r.parse(output));
        let parsed = counts.as_ref().is_some_and(|c| c.recognised);
        let counts = counts.unwrap_or_default();
        let mut details = json!({
            "runner": runner.map(Runner::name),
            "passed": counts.passed,
            "failed": counts.failed,
            "skipped": counts.skipped,
            "exit_code": facts.exit_code,
            "duration_ms": millis(facts.duration),
            "command": facts.command,
            "source": source,
            "parsed": parsed,
        });
        if let Some(unit) = counts.unit {
            details["unit"] = json!(unit);
        }
        if facts.killed {
            details["killed"] = json!(true);
        }
        Self {
            kind: kind::TESTS,
            details,
        }
    }

    /// A `build` outcome: the exit code is the verdict.
    pub fn build(facts: &RunFacts) -> Self {
        let mut details = json!({
            "ok": facts.exit_code == Some(0),
            "exit_code": facts.exit_code,
            "duration_ms": millis(facts.duration),
            "command": facts.command,
        });
        if facts.killed {
            details["killed"] = json!(true);
        }
        Self {
            kind: kind::BUILD,
            details,
        }
    }

    /// A user's mark (`session.mark`).
    pub fn mark(mark: SessionMark, note: Option<&str>) -> Self {
        let mut details = json!({});
        if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
            details["note"] = json!(note);
        }
        Self {
            kind: mark.outcome_kind(),
            details,
        }
    }

    /// An `error` outcome: `kind` names what stopped the run.
    pub fn error(kind: &str, extra: &Value) -> Self {
        let mut details = json!({ "kind": kind });
        if let Some(extra) = extra.as_object() {
            for (k, v) in extra {
                details[k] = v.clone();
            }
        }
        Self {
            kind: kind::ERROR,
            details,
        }
    }

    /// The `reverted` outcome of an earlier run whose changes the next
    /// run found undone: `files` is what came back, `at_agent` the run
    /// that noticed.
    pub fn reverted(files: &[String], at_agent: &AgentId) -> Self {
        Self {
            kind: kind::REVERTED,
            details: json!({ "files": files, "at_agent": at_agent }),
        }
    }

    /// The outcome's payload: `{kind, details}`.
    pub fn payload(&self) -> Value {
        json!({ "kind": self.kind, "details": self.details })
    }

    /// The `outcome` event on an agent (and its step when at one).
    pub fn event(&self, session: SessionId, agent: AgentId) -> NewEvent {
        NewEvent::new(session, kinds::OUTCOME)
            .agent(agent)
            .payload(self.payload())
    }

    /// The `outcome` event at a step.
    pub fn event_at(&self, at: &StepRef) -> NewEvent {
        at.event(kinds::OUTCOME).payload(self.payload())
    }

    /// The one-line summary and the verdict (see [`describe`]).
    pub fn describe(&self) -> (String, Option<bool>) {
        describe(self.kind, &self.details)
    }

    /// The live event clients get once the outcome is recorded.
    pub fn live_event(&self, agent: &AgentId, event_id: &str) -> Event {
        let (summary, ok) = self.describe();
        Event::AgentOutcome {
            agent_id: agent.to_string(),
            event_id: event_id.to_owned(),
            kind: self.kind.to_owned(),
            summary,
            ok,
            details: self.details.clone(),
        }
    }
}

/// The client-facing view of a recorded outcome payload.
pub fn info(event_id: &str, payload: &Value, at: &str) -> OutcomeInfo {
    let kind = payload["kind"].as_str().unwrap_or("?").to_owned();
    let details = payload
        .get("details")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let (summary, ok) = describe(&kind, &details);
    OutcomeInfo {
        event_id: event_id.to_owned(),
        kind,
        summary,
        ok,
        details,
        at: at.to_owned(),
    }
}

/// One line for an outcome and its verdict: `true` for a passing test
/// run, a successful build, an accept or a done mark; `false` for a
/// failing run, a reject, an error, a revert; `None` where the kind
/// carries no verdict (`files_changed`).
pub fn describe(kind: &str, d: &Value) -> (String, Option<bool>) {
    let n = |key: &str| d[key].as_u64().unwrap_or(0);
    let command = || {
        d["command"]
            .as_str()
            .map(|c| format!(" ({})", one_line(c, 60)))
            .unwrap_or_default()
    };
    let exit = |ok_word: &str| match d["exit_code"].as_i64() {
        Some(0) => ok_word.to_owned(),
        Some(code) => format!("exit {code}"),
        None if d["killed"].as_bool() == Some(true) => "killed".to_owned(),
        None => "no exit status".to_owned(),
    };
    match kind {
        kind::FILES_CHANGED => {
            let counts = &d["counts"];
            let get = |key: &str| {
                counts[key]
                    .as_u64()
                    .unwrap_or_else(|| d[key].as_array().map_or(0, Vec::len) as u64)
            };
            let parts: Vec<String> = [
                (get("changed"), "changed"),
                (get("added"), "added"),
                (get("deleted"), "deleted"),
            ]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
            let total = get("changed") + get("added") + get("deleted");
            let plural = if total == 1 { "" } else { "s" };
            match parts.as_slice() {
                [] => ("no files changed".to_owned(), None),
                [one] => (one.replacen(' ', &format!(" file{plural} "), 1), None),
                many => (format!("{total} file{plural} ({})", many.join(", ")), None),
            }
        }
        kind::TESTS => {
            let exit_ok = d["exit_code"].as_i64() == Some(0);
            if d["parsed"].as_bool() == Some(true) {
                let unit = d["unit"].as_str();
                let mut parts = vec![format!("{} passed", n("passed"))];
                if n("failed") > 0 {
                    parts.push(format!("{} failed", n("failed")));
                }
                if n("skipped") > 0 {
                    parts.push(format!("{} skipped", n("skipped")));
                }
                let mut text = parts.join(", ");
                if let Some(unit) = unit {
                    text.push(' ');
                    text.push_str(unit);
                }
                if !exit_ok {
                    text.push_str(", ");
                    text.push_str(&exit("exit 0"));
                }
                (
                    format!("{text}{}", command()),
                    Some(exit_ok && n("failed") == 0),
                )
            } else {
                (
                    format!("tests: {}{}", exit("passed"), command()),
                    Some(exit_ok),
                )
            }
        }
        kind::BUILD => {
            let ok = d["ok"].as_bool().unwrap_or(false);
            (
                format!(
                    "build {}{}",
                    if ok { "ok".to_owned() } else { exit("ok") },
                    command()
                ),
                Some(ok),
            )
        }
        kind::USER_ACCEPT | kind::USER_REJECT | kind::TASK_DONE => {
            let word = match kind {
                kind::USER_ACCEPT => "accepted",
                kind::USER_REJECT => "rejected",
                _ => "task done",
            };
            let text = match d["note"].as_str().filter(|n| !n.is_empty()) {
                Some(note) => format!("{word}: {}", one_line(note, 120)),
                None => word.to_owned(),
            };
            (text, Some(kind != kind::USER_REJECT))
        }
        kind::ERROR => {
            let what = d["kind"].as_str().unwrap_or("error");
            let text = match d["message"].as_str().filter(|m| !m.is_empty()) {
                Some(m) => format!("error: {what} — {}", one_line(m, 120)),
                None => format!("error: {what}"),
            };
            (text, Some(false))
        }
        kind::REVERTED => {
            let files = d["files"].as_array().map_or(0, Vec::len);
            (
                format!("{files} file{} reverted", if files == 1 { "" } else { "s" }),
                Some(false),
            )
        }
        other => (other.to_owned(), None),
    }
}

/// The signal a tool result carries, if any: `run_tests` reports its
/// parsed run in its metadata; a foreground `shell` command that looks
/// like a test or build invocation is read the same way.
pub fn from_tool_result(
    name: &str,
    input: &Value,
    text: &str,
    metadata: &Value,
) -> Option<Outcome> {
    match name {
        "run_tests" => {
            let details = metadata.get("outcome")?.clone();
            Some(Outcome {
                kind: kind::TESTS,
                details,
            })
        }
        "shell" => {
            if input["background"].as_bool() == Some(true) {
                return None;
            }
            let command = input["command"].as_str()?;
            let kind = classify_command(command)?;
            let facts = RunFacts {
                command: command.to_owned(),
                exit_code: metadata["exit_code"]
                    .as_i64()
                    .and_then(|c| i32::try_from(c).ok()),
                duration: Duration::from_millis(metadata["duration_ms"].as_u64().unwrap_or(0)),
                killed: metadata["timed_out"].as_bool() == Some(true)
                    || metadata["killed"].as_bool() == Some(true),
            };
            Some(match kind {
                CommandKind::Tests(runner) => Outcome::tests(runner, text, &facts, "shell"),
                CommandKind::Build => Outcome::build(&facts),
            })
        }
        _ => None,
    }
}

/// The first `max` characters of `text` on one line.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(command: &str, exit_code: Option<i32>) -> RunFacts {
        RunFacts {
            command: command.into(),
            exit_code,
            duration: Duration::from_millis(1234),
            killed: false,
        }
    }

    #[test]
    fn a_parsed_test_run_is_summarised_with_its_verdict() {
        let out = "test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n";
        let o = Outcome::tests(
            Some(Runner::Cargo),
            out,
            &facts("cargo test", Some(0)),
            "shell",
        );
        assert_eq!(o.kind, "tests");
        assert_eq!(o.details["parsed"], true);
        assert_eq!(o.details["runner"], "cargo");
        assert_eq!(
            o.describe(),
            ("3 passed, 1 skipped (cargo test)".into(), Some(true))
        );

        let out = "test result: FAILED. 2 passed; 1 failed; 0 ignored\n";
        let o = Outcome::tests(
            Some(Runner::Cargo),
            out,
            &facts("cargo test", Some(101)),
            "shell",
        );
        assert_eq!(
            o.describe(),
            (
                "2 passed, 1 failed, exit 101 (cargo test)".into(),
                Some(false)
            )
        );

        // Nothing recognised: the exit code is the verdict.
        let o = Outcome::tests(None, "whatever\n", &facts("just test", Some(0)), "shell");
        assert_eq!(o.details["parsed"], false);
        assert_eq!(
            o.describe(),
            ("tests: passed (just test)".into(), Some(true))
        );
        let o = Outcome::tests(
            Some(Runner::Cargo),
            "error[E0308]\n",
            &facts("cargo test", Some(101)),
            "run_tests",
        );
        assert_eq!(
            o.describe(),
            ("tests: exit 101 (cargo test)".into(), Some(false))
        );
    }

    #[test]
    fn builds_marks_errors_and_reverts_describe_themselves() {
        let b = Outcome::build(&facts("cargo build", Some(0)));
        assert_eq!(b.describe(), ("build ok (cargo build)".into(), Some(true)));
        let b = Outcome::build(&facts("tsc --noEmit", Some(2)));
        assert_eq!(
            b.describe(),
            ("build exit 2 (tsc --noEmit)".into(), Some(false))
        );

        let m = Outcome::mark(SessionMark::Accept, Some("  looks right  "));
        assert_eq!(m.kind, "user_accept");
        assert_eq!(m.details["note"], "looks right");
        assert_eq!(m.describe(), ("accepted: looks right".into(), Some(true)));
        let m = Outcome::mark(SessionMark::Reject, Some(""));
        assert_eq!(m.details.get("note"), None);
        assert_eq!(m.describe(), ("rejected".into(), Some(false)));
        assert_eq!(
            Outcome::mark(SessionMark::Done, None).describe(),
            ("task done".into(), Some(true))
        );

        let e = Outcome::error("max_iterations", &json!({ "max_iterations": 2 }));
        assert_eq!(e.details["max_iterations"], 2);
        assert_eq!(e.describe(), ("error: max_iterations".into(), Some(false)));

        let r = Outcome::reverted(&["a.rs".into()], &AgentId::from("a2"));
        assert_eq!(r.describe(), ("1 file reverted".into(), Some(false)));
        assert_eq!(r.details["at_agent"], "a2");
    }

    #[test]
    fn files_changed_payloads_are_summarised_from_their_counts() {
        let (s, ok) = describe(
            "files_changed",
            &json!({ "counts": { "changed": 2, "added": 1, "deleted": 0 } }),
        );
        assert_eq!(s, "3 files (2 changed, 1 added)");
        assert_eq!(ok, None);
        let (s, _) = describe(
            "files_changed",
            &json!({ "changed": ["a"], "added": [], "deleted": [] }),
        );
        assert_eq!(s, "1 file changed");
        let (s, _) = describe("files_changed", &json!({ "counts": {} }));
        assert_eq!(s, "no files changed");
        let i = info(
            "e1",
            &json!({ "kind": "future", "details": { "x": 1 } }),
            "t",
        );
        assert_eq!((i.summary.as_str(), i.ok), ("future", None));
        assert_eq!(i.details["x"], 1);
    }

    #[test]
    fn tool_results_yield_outcomes_for_test_and_build_commands_only() {
        let meta = json!({ "exit_code": 0, "duration_ms": 50 });
        let o = from_tool_result(
            "shell",
            &json!({ "command": "cargo test -p demo" }),
            "test result: ok. 1 passed; 0 failed; 0 ignored\n",
            &meta,
        )
        .unwrap();
        assert_eq!(o.kind, "tests");
        assert_eq!(o.details["source"], "shell");
        assert_eq!(o.details["passed"], 1);
        let o = from_tool_result("shell", &json!({ "command": "cargo build" }), "", &meta).unwrap();
        assert_eq!(o.kind, "build");
        assert_eq!(o.details["ok"], true);
        assert!(from_tool_result("shell", &json!({ "command": "ls" }), "", &meta).is_none());
        assert!(
            from_tool_result(
                "shell",
                &json!({ "command": "cargo test", "background": true }),
                "",
                &meta
            )
            .is_none()
        );
        assert!(from_tool_result("read_file", &json!({}), "", &meta).is_none());
        let o = from_tool_result(
            "run_tests",
            &json!({}),
            "",
            &json!({ "outcome": { "runner": "go", "passed": 4, "failed": 0, "skipped": 0, "exit_code": 0, "parsed": true, "command": "go test ./..." } }),
        )
        .unwrap();
        assert_eq!(
            o.describe(),
            ("4 passed (go test ./...)".into(), Some(true))
        );
        let killed = json!({ "exit_code": null, "duration_ms": 5, "timed_out": true });
        let o = from_tool_result("shell", &json!({ "command": "pytest" }), "", &killed).unwrap();
        assert_eq!(o.details["killed"], true);
        assert_eq!(o.describe(), ("tests: killed (pytest)".into(), Some(false)));
    }
}
