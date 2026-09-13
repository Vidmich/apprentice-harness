//! Golden tests for the test-runner parsers (task M01-15): recorded
//! outputs of cargo, nextest, pytest, vitest, jest and go under
//! `tests/fixtures/runners/`, each read by its parser into the counts a
//! `tests` outcome records.

use std::path::Path;
use std::time::Duration;

use apprentice_core::outcomes::{Outcome, RunFacts, Runner};

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/runners")
        .join(format!("{name}.txt"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The exit code recorded at the end of each fixture (`exit N · ...`).
fn exit_code(output: &str) -> Option<i32> {
    output
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("exit "))
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
}

#[test]
fn recorded_outputs_parse_into_the_same_counts() {
    let cases = [
        ("cargo_ok", Runner::Cargo, "cargo test"),
        ("cargo_failed", Runner::Cargo, "cargo test"),
        ("cargo_compile_error", Runner::Cargo, "cargo test"),
        ("nextest", Runner::Cargo, "cargo nextest run"),
        ("pytest_ok", Runner::Pytest, "uv run pytest"),
        ("pytest_failed", Runner::Pytest, "pytest"),
        ("pytest_no_tests", Runner::Pytest, "pytest tests/none"),
        ("vitest_ok", Runner::Node, "pnpm test"),
        ("vitest_failed", Runner::Node, "pnpm test"),
        ("jest", Runner::Node, "npx jest"),
        ("go_verbose", Runner::Go, "go test -v ./..."),
        ("go_packages", Runner::Go, "go test ./..."),
        ("go_packages_quiet", Runner::Go, "go test ./..."),
    ];
    let mut golden = serde_json::Map::new();
    for (name, runner, command) in cases {
        let output = fixture(name);
        let facts = RunFacts {
            command: command.into(),
            exit_code: exit_code(&output),
            duration: Duration::from_millis(1500),
            killed: false,
        };
        let outcome = Outcome::tests(Some(runner), &output, &facts, "shell");
        let (summary, ok) = outcome.describe();
        golden.insert(
            name.to_owned(),
            serde_json::json!({
                "counts": runner.parse(&output),
                "summary": summary,
                "ok": ok,
                "details": outcome.details,
            }),
        );
    }
    insta::assert_json_snapshot!("runner_outputs", golden);
}

#[test]
fn the_verdict_follows_the_counts_and_the_exit_code() {
    let pass = |name: &str, runner: Runner, want: (u64, u64, u64)| {
        let c = runner.parse(&fixture(name));
        assert!(c.recognised, "{name}");
        assert_eq!((c.passed, c.failed, c.skipped), want, "{name}");
    };
    pass("cargo_ok", Runner::Cargo, (5, 0, 1));
    pass("cargo_failed", Runner::Cargo, (1, 1, 1));
    pass("nextest", Runner::Cargo, (3, 1, 1));
    pass("pytest_ok", Runner::Pytest, (8, 0, 1));
    // errors count as failures, xfailed as skipped, xpassed as passed.
    pass("pytest_failed", Runner::Pytest, (9, 2, 1));
    pass("pytest_no_tests", Runner::Pytest, (0, 0, 0));
    pass("vitest_ok", Runner::Node, (11, 0, 1));
    pass("vitest_failed", Runner::Node, (9, 1, 0));
    pass("jest", Runner::Node, (7, 1, 2));
    pass("go_verbose", Runner::Go, (3, 1, 1));
    // Without -v the packages are counted; a `--- FAIL` line anywhere
    // switches to tests.
    pass("go_packages", Runner::Go, (0, 1, 0));
    let quiet = Runner::Go.parse(&fixture("go_packages_quiet"));
    assert_eq!((quiet.passed, quiet.failed, quiet.skipped), (2, 1, 1));
    assert_eq!(quiet.unit, Some("packages"));
    assert!(
        !Runner::Cargo
            .parse(&fixture("cargo_compile_error"))
            .recognised
    );
}
