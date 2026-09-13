//! `run_tests` (task M01-15): the workspace's test suite as one call.
//! Without a `command` the runner is found from the manifest files of
//! `cwd` (`Cargo.toml` → `cargo test`, `package.json` with a `test`
//! script → `pnpm test`, `pyproject.toml` → `uv run pytest`, `go.mod`
//! → `go test ./...`); the run goes through the shell machinery of
//! [`super::Shell`] — same program, environment, capture, timeout —
//! and the output is read by the runner's parser into a `tests`
//! outcome the runtime records. The mentor gets the usual transcript
//! with one parsed line on top: `[tests] 12 passed, 1 failed (cargo
//! test · exit 101 · 2.4 s)`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::capture::{Reporter, Transcript};
use super::process::{Ended, spawn};
use super::program::Program;
use super::{SHELL_MAX_TIMEOUT_S, WRAPPER_TIMEOUT, render};
use crate::outcomes::{CommandKind, Outcome, RunFacts, Runner, classify_command, detect_runner};
use crate::tools::file::{parse, target};
use crate::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

/// `timeout_s` when the mentor gives none: suites take longer than a
/// command.
pub const RUN_TESTS_DEFAULT_TIMEOUT_S: u64 = 600;

const DESCRIPTION: &str = "Runs the workspace's tests and reports the parsed result.

Without `command`, the test runner is detected from the files in `cwd` (default: the workspace root): `Cargo.toml` → `cargo test`, `package.json` with a `test` script → `pnpm test` (`npm`/`yarn` by lockfile), `pyproject.toml` → `uv run pytest` (or `pytest`), `go.mod` → `go test ./...`. The result says which command ran. Pass `command` to run something else (`cargo test -p core --test loop`, `pytest tests/test_x.py -x`); `runner` names the parser when the command does not make it obvious.

The output is the command's transcript, as `shell` returns it, with one line on top: `[tests] <passed> passed, <failed> failed, <skipped> skipped (<command> · exit <code> · <duration>)` — or `[tests] no summary recognised` when the output has none the parser knows (a compile error, an unknown runner), in which case the exit code is the verdict. The counts are recorded as the run's outcome.

Prefer this over `shell` for running tests: the parsed result is what the user sees at a glance.";

/// `run_tests`.
#[derive(Debug, Default)]
pub struct RunTests;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: Option<String>,
    runner: Option<Runner>,
    cwd: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_s: u64,
    description: Option<String>,
}

fn default_timeout() -> u64 {
    RUN_TESTS_DEFAULT_TIMEOUT_S
}

#[async_trait]
impl Tool for RunTests {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "run_tests",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The test command to run instead of the detected one."
                    },
                    "runner": {
                        "type": "string",
                        "enum": ["cargo", "pytest", "node", "go"],
                        "description": "Which parser reads the output (default: from the command)."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Directory to run in, relative to the workspace root (default: the root)."
                    },
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": SHELL_MAX_TIMEOUT_S,
                        "default": RUN_TESTS_DEFAULT_TIMEOUT_S,
                        "description": "Seconds before the run and its process tree are killed."
                    },
                    "description": {
                        "type": "string",
                        "maxLength": 200,
                        "description": "One line for the user saying what is being checked."
                    }
                },
                "additionalProperties": false
            }),
            Risk::Execute,
        )
        .with_tags(["shell", "tests"])
        .with_timeout(WRAPPER_TIMEOUT)
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input = parse(input)?;
        let (_, t) = target(ctx, input.cwd.as_deref().unwrap_or("."))?;
        if !t.abs.is_dir() {
            return Ok(ToolOutput::error(format!(
                "`{}` is not a directory in the workspace",
                t.shown
            )));
        }
        let (command, runner, detected) = match input.command {
            Some(command) => {
                let runner = input.runner.or_else(|| match classify_command(&command) {
                    Some(CommandKind::Tests(r)) => r,
                    _ => None,
                });
                (command, runner, None)
            }
            None => match detect_runner(&t.abs) {
                Some(d) => (
                    d.command,
                    Some(input.runner.unwrap_or(d.runner)),
                    Some(d.because),
                ),
                None => {
                    return Ok(ToolOutput::error(format!(
                        "no test runner recognised in `{}`: no Cargo.toml, package.json with a \
                         `test` script, pyproject.toml or go.mod there; pass `command`",
                        t.shown
                    )));
                }
            },
        };

        let config = &ctx.config.shell;
        let program = Program::resolve(config);
        let timeout = Duration::from_secs(
            input
                .timeout_s
                .min(config.max_timeout_s)
                .clamp(1, SHELL_MAX_TIMEOUT_S),
        );
        let spawned = match spawn(&program, config, &command, &t.abs, true) {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "cannot start `{}`: {e}",
                    program.path.display()
                )));
            }
        };
        let mut transcript = Transcript::new(ctx.env.max_capture_bytes);
        let mut reporter = Reporter::new(ctx);
        let outcome = super::process::pump(spawned, timeout, &cancel, |stream, bytes| {
            transcript.push(stream, bytes);
            reporter.push(stream, String::from_utf8_lossy(bytes).into_owned());
        })
        .await;
        reporter.flush();
        if outcome.ended == Ended::Killed {
            return Err(ToolError::Cancelled);
        }

        let mut out = render(&transcript, &outcome, timeout);
        let facts = RunFacts {
            command: command.clone(),
            exit_code: out.metadata["exit_code"]
                .as_i64()
                .and_then(|c| i32::try_from(c).ok()),
            duration: outcome.duration,
            killed: outcome.ended == Ended::TimedOut,
        };
        let signal = Outcome::tests(runner, &transcript.text(), &facts, "run_tests");
        let (line, _) = signal.describe();
        let headline = if signal.details["parsed"].as_bool() == Some(true) {
            format!("[tests] {line}")
        } else {
            format!("[tests] no summary recognised; {line}")
        };
        let body = match &mut out.content {
            crate::tools::ToolContent::Text(text) => std::mem::take(text),
            _ => String::new(),
        };
        out.content = crate::tools::ToolContent::Text(format!("{headline}\n{body}"));
        out.summary = line;
        out.metadata["shell"] = json!(program.name);
        out.metadata["description"] = json!(input.description);
        out.metadata["command"] = json!(command);
        out.metadata["runner"] = json!(runner.map(Runner::name));
        out.metadata["detected_from"] = json!(detected);
        out.metadata["outcome"] = signal.details;
        if reporter.dropped_lines() > 0 {
            out.metadata["progress_dropped_lines"] = json!(reporter.dropped_lines());
        }
        Ok(out)
    }
}
