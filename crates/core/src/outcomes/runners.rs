//! Test runners (task M01-15): which one a workspace uses
//! ([`detect_runner`]), which one a shell command invokes
//! ([`classify_command`]), and how each one's summary is read
//! ([`Runner::parse`]). Parsers look for the runner's closing summary
//! lines only — the part every run prints whatever went before — and
//! say when they found none, so an unrecognised output is an
//! exit-code-only outcome rather than a false zero.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// A test runner with a parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runner {
    /// `cargo test` (libtest summaries) and `cargo nextest`.
    Cargo,
    /// pytest's closing `=== N passed ... in Ns ===` line.
    Pytest,
    /// vitest (`Tests  N passed | M failed (T)`) and jest
    /// (`Tests: N failed, M passed, T total`).
    Node,
    /// `go test`: `--- PASS/FAIL/SKIP` lines with `-v`, else the
    /// per-package `ok` / `FAIL` lines.
    Go,
}

impl Runner {
    pub const ALL: &'static [Self] = &[Self::Cargo, Self::Pytest, Self::Node, Self::Go];

    /// The `runner` name in outcomes and tool inputs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Pytest => "pytest",
            Self::Node => "node",
            Self::Go => "go",
        }
    }

    /// Parses a `runner` name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.name() == name)
    }

    /// Reads the runner's summary out of `output`.
    pub fn parse(self, output: &str) -> TestCounts {
        match self {
            Self::Cargo => parse_cargo(output),
            Self::Pytest => parse_pytest(output),
            Self::Node => parse_node(output),
            Self::Go => parse_go(output),
        }
    }
}

/// What a runner's summary said.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    /// A summary the parser knows was found; the counts are meaningless
    /// otherwise.
    pub recognised: bool,
    /// What was counted when it is not tests (`packages` for a `go
    /// test` without `-v`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
}

/// The runner a workspace directory uses, with the command to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub runner: Runner,
    pub command: String,
    /// The file that gave it away.
    pub because: &'static str,
}

/// Finds the runner of `dir` from its manifest files, in this order:
/// `Cargo.toml`, `package.json` with a `test` script, `pyproject.toml`,
/// `go.mod`. Node picks the package manager from the lockfile (pnpm by
/// default); Python uses `uv run pytest` under a `uv.lock`, `pytest`
/// otherwise.
pub fn detect_runner(dir: &Path) -> Option<Detected> {
    if dir.join("Cargo.toml").is_file() {
        return Some(Detected {
            runner: Runner::Cargo,
            command: "cargo test".into(),
            because: "Cargo.toml",
        });
    }
    if has_test_script(&dir.join("package.json")) {
        let pm = if dir.join("yarn.lock").is_file() {
            "yarn"
        } else if dir.join("package-lock.json").is_file() {
            "npm"
        } else {
            "pnpm"
        };
        return Some(Detected {
            runner: Runner::Node,
            command: format!("{pm} test"),
            because: "package.json",
        });
    }
    if dir.join("pyproject.toml").is_file() {
        let command = if dir.join("uv.lock").is_file() {
            "uv run pytest"
        } else {
            "pytest"
        };
        return Some(Detected {
            runner: Runner::Pytest,
            command: command.into(),
            because: "pyproject.toml",
        });
    }
    if dir.join("go.mod").is_file() {
        return Some(Detected {
            runner: Runner::Go,
            command: "go test ./...".into(),
            because: "go.mod",
        });
    }
    None
}

/// `package.json` names a `scripts.test`.
fn has_test_script(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v["scripts"]["test"].is_string())
}

/// What a shell command is, when it is a test or build invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// A test run; `None` for a runner without a parser (`just test`).
    Tests(Option<Runner>),
    Build,
}

/// Recognises test and build invocations in a command line. A chain
/// (`&&`, `;`, `||`, newlines) is read segment by segment: a test run
/// anywhere makes it a test command (the runner of the last one),
/// else a build anywhere makes it a build.
pub fn classify_command(command: &str) -> Option<CommandKind> {
    let mut tests: Option<Option<Runner>> = None;
    let mut build = false;
    for segment in split_chain(command) {
        match classify_segment(segment) {
            Some(CommandKind::Tests(r)) => tests = Some(r),
            Some(CommandKind::Build) => build = true,
            None => {}
        }
    }
    match tests {
        Some(r) => Some(CommandKind::Tests(r)),
        None if build => Some(CommandKind::Build),
        None => None,
    }
}

fn split_chain(command: &str) -> impl Iterator<Item = &str> {
    command
        .split(['\n', ';'])
        .flat_map(|s| s.split("&&"))
        .flat_map(|s| s.split("||"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The words of one command, leading environment assignments
/// (`RUST_LOG=debug cargo test`) stripped.
fn words(segment: &str) -> Vec<&str> {
    segment
        .split_whitespace()
        .skip_while(|w| is_assignment(w))
        .collect()
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

fn classify_segment(segment: &str) -> Option<CommandKind> {
    let w = words(segment);
    let (first, rest) = w.split_first()?;
    let first = Path::new(first)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(first);
    let first = first.strip_suffix(".exe").unwrap_or(first);
    // The first word the family knows, wherever it is: options (with
    // their values) and toolchain selectors come before it.
    let sub = |known: &[&str]| rest.iter().copied().find(|a| known.contains(a));
    let has = |flag: &str| rest.contains(&flag);
    let pytest = || {
        if has("--collect-only") || has("--co") {
            CommandKind::Build
        } else {
            CommandKind::Tests(Some(Runner::Pytest))
        }
    };
    Some(match first {
        "cargo" => match sub(&[
            "test", "t", "nextest", "build", "b", "check", "c", "clippy", "run", "r", "fmt", "doc",
            "bench", "install", "publish", "add", "remove", "update", "new", "init", "clean",
        ]) {
            Some("test" | "t" | "nextest") => CommandKind::Tests(Some(Runner::Cargo)),
            Some("build" | "b" | "check" | "c" | "clippy") => CommandKind::Build,
            _ => return None,
        },
        "pytest" | "py.test" => pytest(),
        "python" | "python3" | "py" => {
            if rest.windows(2).any(|p| p == ["-m", "pytest"]) {
                pytest()
            } else {
                return None;
            }
        }
        "uv" | "poetry" | "pipenv" | "hatch" | "pdm" => {
            // `uv run [options] pytest ...`, `poetry run python -m pytest`:
            // the first tail after `run` that is a command we know.
            let start = rest.iter().position(|a| *a == "run")? + 1;
            return (start..rest.len()).find_map(|i| classify_segment(&rest[i..].join(" ")));
        }
        "go" => match sub(&[
            "test", "build", "vet", "run", "generate", "mod", "get", "install",
        ]) {
            Some("test") => CommandKind::Tests(Some(Runner::Go)),
            Some("build" | "vet") => CommandKind::Build,
            _ => return None,
        },
        "pnpm" | "npm" | "yarn" | "bun" => match sub(&[
            "test",
            "t",
            "vitest",
            "jest",
            "run",
            "build",
            "typecheck",
            "install",
            "i",
            "add",
            "exec",
            "dlx",
            "start",
            "dev",
        ]) {
            Some("test" | "t" | "vitest" | "jest") => CommandKind::Tests(Some(Runner::Node)),
            Some("run") => match rest.iter().skip_while(|a| **a != "run").nth(1) {
                Some(&("test" | "test:unit" | "test:ci" | "vitest" | "jest")) => {
                    CommandKind::Tests(Some(Runner::Node))
                }
                Some(&("build" | "typecheck" | "lint" | "check")) => CommandKind::Build,
                _ => return None,
            },
            Some("build" | "typecheck") => CommandKind::Build,
            _ => return None,
        },
        "npx" | "pnpx" | "bunx" => match sub(&["vitest", "jest", "tsc"]) {
            Some("vitest" | "jest") => CommandKind::Tests(Some(Runner::Node)),
            Some("tsc") => CommandKind::Build,
            _ => return None,
        },
        "vitest" | "jest" => CommandKind::Tests(Some(Runner::Node)),
        "tsc" => CommandKind::Build,
        "just" | "make" => match sub(&["test", "tests", "build", "check"]) {
            Some("test" | "tests") => CommandKind::Tests(None),
            Some("build" | "check") => CommandKind::Build,
            _ => return None,
        },
        _ => return None,
    })
}

// ------------------------------------------------------------ parsers

/// libtest: `test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured;
/// 0 filtered out; finished in 0.01s` — one line per test binary,
/// summed. nextest: `Summary [ 0.123s] 3 tests run: 3 passed, 0 skipped`.
fn parse_cargo(output: &str) -> TestCounts {
    static LIBTEST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored")
            .expect("libtest regex")
    });
    static NEXTEST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"Summary \[[^\]]*\]\s+(\d+) tests? run: (\d+) passed(?:, (\d+) failed)?(?:, (\d+) skipped)?")
            .expect("nextest regex")
    });
    let mut counts = TestCounts::default();
    for c in LIBTEST.captures_iter(output) {
        counts.recognised = true;
        counts.passed += num(&c, 1);
        counts.failed += num(&c, 2);
        counts.skipped += num(&c, 3);
    }
    if !counts.recognised
        && let Some(c) = NEXTEST.captures_iter(output).last()
    {
        counts.recognised = true;
        counts.passed = num(&c, 2);
        counts.failed = num(&c, 3);
        counts.skipped = num(&c, 4);
    }
    counts
}

/// pytest's last summary line: `==== 3 passed, 1 failed, 2 skipped,
/// 1 error, 2 warnings in 0.12s ====`, `= 5 passed in 0.03s =`, or
/// `no tests ran`. Errors count as failures, `xfailed` as skipped,
/// `xpassed` as passed.
fn parse_pytest(output: &str) -> TestCounts {
    static SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^=+ (.+?) in [\d.]+ ?s ?(?:\([^)]*\))? =+$").expect("pytest regex")
    });
    static PART: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d+) (passed|failed|skipped|errors?|xfailed|xpassed|deselected|warnings?)")
            .expect("pytest part regex")
    });
    let mut counts = TestCounts::default();
    let Some(line) = SUMMARY.captures_iter(output).last() else {
        return counts;
    };
    let body = line.get(1).map_or("", |m| m.as_str());
    counts.recognised = true;
    for c in PART.captures_iter(body) {
        let n = num(&c, 1);
        match &c[2] {
            "passed" | "xpassed" => counts.passed += n,
            "failed" | "error" | "errors" => counts.failed += n,
            "skipped" | "xfailed" => counts.skipped += n,
            _ => {}
        }
    }
    counts
}

/// vitest: `      Tests  2 passed | 1 skipped (3)`; jest: `Tests:
/// 1 failed, 2 passed, 3 total`. The last such line wins.
fn parse_node(output: &str) -> TestCounts {
    static LINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*Tests:?\s+(.+)$").expect("node tests regex"));
    static PART: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d+) (passed|failed|skipped|todo|pending|total)").expect("node part regex")
    });
    let mut counts = TestCounts::default();
    let Some(line) = LINE
        .captures_iter(output)
        .filter(|c| PART.is_match(&c[1]))
        .last()
    else {
        return counts;
    };
    counts.recognised = true;
    for c in PART.captures_iter(&line[1]) {
        let n = num(&c, 1);
        match &c[2] {
            "passed" => counts.passed += n,
            "failed" => counts.failed += n,
            "skipped" | "todo" | "pending" => counts.skipped += n,
            _ => {}
        }
    }
    counts
}

/// `go test`: with `-v`, `--- PASS: TestX (0.00s)`, `--- FAIL:`,
/// `--- SKIP:` per test; without, `ok  \tpkg\t0.01s`, `FAIL\tpkg`,
/// `?   \tpkg\t[no test files]` per package (`unit: packages`).
fn parse_go(output: &str) -> TestCounts {
    static TEST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*--- (PASS|FAIL|SKIP): ").expect("go test regex"));
    static PACKAGE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^(ok|FAIL|\?)\s+\S+(?:\s+.*)?$").expect("go package regex")
    });
    let mut counts = TestCounts::default();
    for c in TEST.captures_iter(output) {
        counts.recognised = true;
        match &c[1] {
            "PASS" => counts.passed += 1,
            "FAIL" => counts.failed += 1,
            _ => counts.skipped += 1,
        }
    }
    if counts.recognised {
        return counts;
    }
    for c in PACKAGE.captures_iter(output) {
        counts.recognised = true;
        counts.unit = Some("packages");
        match &c[1] {
            "ok" => counts.passed += 1,
            "FAIL" => counts.failed += 1,
            _ => counts.skipped += 1,
        }
    }
    counts
}

fn num(c: &regex::Captures<'_>, i: usize) -> u64 {
    c.get(i).and_then(|m| m.as_str().parse().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_classified_by_their_first_word_and_subcommand() {
        let tests = |c: &str, r: Option<Runner>| {
            assert_eq!(classify_command(c), Some(CommandKind::Tests(r)), "{c}");
        };
        let build = |c: &str| assert_eq!(classify_command(c), Some(CommandKind::Build), "{c}");
        let none = |c: &str| assert_eq!(classify_command(c), None, "{c}");
        tests("cargo test", Some(Runner::Cargo));
        tests(
            "cargo test -p apprentice-core --test loop",
            Some(Runner::Cargo),
        );
        tests("cargo +nightly test", Some(Runner::Cargo));
        tests("cargo nextest run", Some(Runner::Cargo));
        tests("RUST_LOG=debug cargo t", Some(Runner::Cargo));
        tests("pytest -q tests/", Some(Runner::Pytest));
        tests("python -m pytest", Some(Runner::Pytest));
        tests("uv run pytest -x", Some(Runner::Pytest));
        tests("uv run --directory ml pytest", Some(Runner::Pytest));
        tests("poetry run python -m pytest", Some(Runner::Pytest));
        tests("pnpm test", Some(Runner::Node));
        tests("pnpm --dir apps/gui test", Some(Runner::Node));
        tests("npm test", Some(Runner::Node));
        tests("npm run test", Some(Runner::Node));
        tests("yarn test", Some(Runner::Node));
        tests("npx vitest run", Some(Runner::Node));
        tests("pnpm vitest", Some(Runner::Node));
        tests("go test ./...", Some(Runner::Go));
        tests("go test -v ./pkg/...", Some(Runner::Go));
        tests("just test", None);
        tests("make test", None);
        tests("cargo build && cargo test", Some(Runner::Cargo));
        tests("cd apps/gui && pnpm test", Some(Runner::Node));
        tests("cargo test; go test ./...", Some(Runner::Go));
        build("cargo build --release");
        build("cargo check");
        build("cargo clippy --all-targets");
        build("cargo build || echo failed");
        build("tsc --noEmit");
        build("npx tsc -p .");
        build("pnpm build");
        build("pnpm run typecheck");
        build("npm run build");
        build("go build ./...");
        build("go vet ./...");
        build("pytest --collect-only -q");
        build("uv run pytest --co");
        build("just check");
        none("cargo run");
        none("cargo fmt --check");
        none("ls -la");
        none("git status");
        none("python script.py");
        none("pnpm install");
        none("go run main.go");
        none("");
    }

    #[test]
    fn workspace_manifests_pick_the_runner() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(detect_runner(root), None);
        std::fs::write(root.join("go.mod"), "module x\n").unwrap();
        let d = detect_runner(root).unwrap();
        assert_eq!(
            (d.runner, d.command.as_str(), d.because),
            (Runner::Go, "go test ./...", "go.mod")
        );
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        let d = detect_runner(root).unwrap();
        assert_eq!((d.runner, d.command.as_str()), (Runner::Pytest, "pytest"));
        std::fs::write(root.join("uv.lock"), "").unwrap();
        assert_eq!(detect_runner(root).unwrap().command, "uv run pytest");
        // No test script: not a node project for this purpose.
        std::fs::write(root.join("package.json"), r#"{"name": "x"}"#).unwrap();
        assert_eq!(detect_runner(root).unwrap().runner, Runner::Pytest);
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts": {"test": "vitest run"}}"#,
        )
        .unwrap();
        let d = detect_runner(root).unwrap();
        assert_eq!(
            (d.runner, d.command.as_str(), d.because),
            (Runner::Node, "pnpm test", "package.json")
        );
        std::fs::write(root.join("package-lock.json"), "{}").unwrap();
        assert_eq!(detect_runner(root).unwrap().command, "npm test");
        std::fs::write(root.join("yarn.lock"), "").unwrap();
        assert_eq!(detect_runner(root).unwrap().command, "yarn test");
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let d = detect_runner(root).unwrap();
        assert_eq!(
            (d.runner, d.command.as_str(), d.because),
            (Runner::Cargo, "cargo test", "Cargo.toml")
        );
        assert_eq!(Runner::from_name("go"), Some(Runner::Go));
        assert_eq!(Runner::from_name("mocha"), None);
    }

    #[test]
    fn unrecognised_output_is_not_a_zero() {
        for r in Runner::ALL {
            let c = r.parse("error: could not compile\n");
            assert!(!c.recognised, "{r:?}");
            assert_eq!((c.passed, c.failed, c.skipped), (0, 0, 0));
        }
    }
}
