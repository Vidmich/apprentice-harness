//! `harness stats tokens | calls | reprice` (tasks M00-07, M01-13).
//! Parsing of `--since` / `--until` happens in the daemon so the CLI and
//! GUI agree on it; the values are passed through verbatim.

use std::fmt::Write as _;

use apprentice_api::methods::{
    StatsCalls, StatsCallsParams, StatsCallsResult, StatsOutcomes, StatsOutcomesParams,
    StatsReprice, StatsRepriceParams, StatsTokens, StatsTokensParams,
};
use apprentice_api::types::{CallSummary, OutcomeStats, StatsGroup, TokenBucket, TokenStats};
use clap::{Args, Subcommand, ValueEnum};

use crate::Ctx;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum StatsCommand {
    /// Mentor token usage and cost, aggregated from the trace store.
    Tokens(TokensArgs),
    /// The individual mentor calls behind the numbers, newest first.
    Calls(CallsArgs),
    /// Recompute stored call costs from the current `[pricing]` config.
    Reprice(RepriceArgs),
    /// Outcome signals of the runs in a range: how many are labelled.
    Outcomes(OutcomesArgs),
}

#[derive(Debug, Args)]
pub struct OutcomesArgs {
    /// Start of the range (an age, a date or an RFC 3339 timestamp).
    #[arg(long, value_name = "WHEN")]
    since: Option<String>,
    /// End of the range (exclusive; a bare date includes that whole day).
    #[arg(long, value_name = "WHEN")]
    until: Option<String>,
    /// Restrict to the sessions of one workspace (its registry id).
    #[arg(long, value_name = "ID")]
    workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct TokensArgs {
    /// Start of the range: an age (7d, 12h, 30m, 2w), a date (2026-09-01)
    /// or an RFC 3339 timestamp.
    #[arg(long, value_name = "WHEN")]
    since: Option<String>,
    /// End of the range (exclusive; a bare date includes that whole day).
    #[arg(long, value_name = "WHEN")]
    until: Option<String>,
    /// Restrict to one session.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
    /// Restrict to the sessions of one workspace (its registry id).
    #[arg(long, value_name = "ID")]
    workspace: Option<String>,
    /// Breakdown table(s) to print; repeatable. Default: model.
    #[arg(long, value_enum, value_name = "GROUP")]
    by: Vec<By>,
}

#[derive(Debug, Args)]
pub struct CallsArgs {
    /// Start of the range (an age, a date or an RFC 3339 timestamp).
    #[arg(long, value_name = "WHEN")]
    since: Option<String>,
    /// End of the range (exclusive; a bare date includes that whole day).
    #[arg(long, value_name = "WHEN")]
    until: Option<String>,
    /// Restrict to one session.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
    /// Restrict to the sessions of one workspace (its registry id).
    #[arg(long, value_name = "ID")]
    workspace: Option<String>,
    /// Rows to print (newest first); at most 1000 per call.
    #[arg(long, value_name = "N", default_value_t = 100)]
    limit: u32,
    /// Rows to skip, for paging.
    #[arg(long, value_name = "N", default_value_t = 0)]
    offset: u64,
    /// Print the rows as CSV (the columns of the GUI's export).
    #[arg(long)]
    csv: bool,
}

#[derive(Debug, Args)]
pub struct RepriceArgs {
    /// Only calls of this model.
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    #[arg(long, value_name = "WHEN")]
    since: Option<String>,
    #[arg(long, value_name = "WHEN")]
    until: Option<String>,
    #[arg(long, value_name = "ID")]
    session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum By {
    Model,
    Day,
    Session,
    Workspace,
    Kind,
}

impl From<By> for StatsGroup {
    fn from(by: By) -> Self {
        match by {
            By::Model => Self::Model,
            By::Day => Self::Day,
            By::Session => Self::Session,
            By::Workspace => Self::Workspace,
            By::Kind => Self::Kind,
        }
    }
}

pub fn run(ctx: &Ctx, cmd: &StatsCommand) -> anyhow::Result<()> {
    let json = ctx.out.json;
    match cmd {
        StatsCommand::Tokens(a) => {
            let by = if a.by.is_empty() {
                &[By::Model][..]
            } else {
                &a.by
            };
            let params = StatsTokensParams {
                since: a.since.clone(),
                until: a.until.clone(),
                session_id: a.session.clone(),
                workspace_id: a.workspace.clone(),
                // `--json` keeps the M00-07 tables; a human gets the
                // ones asked for.
                group_by: if json {
                    Vec::new()
                } else {
                    by.iter().copied().map(StatsGroup::from).collect()
                },
            };
            let stats = with_client(
                ctx,
                |c| async move { Ok(c.call::<StatsTokens>(params).await?) },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                print!("{}", format_token_stats(&stats, by));
            }
        }
        StatsCommand::Calls(a) => {
            let params = StatsCallsParams {
                since: a.since.clone(),
                until: a.until.clone(),
                session_id: a.session.clone(),
                workspace_id: a.workspace.clone(),
                limit: Some(a.limit),
                offset: a.offset,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<StatsCalls>(params).await?) },
            )?;
            if a.csv {
                print!("{}", calls_csv(&r.calls));
            } else if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", format_calls(&r, a.offset));
            }
        }
        StatsCommand::Outcomes(a) => {
            let params = StatsOutcomesParams {
                since: a.since.clone(),
                until: a.until.clone(),
                workspace_id: a.workspace.clone(),
            };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<StatsOutcomes>(params).await?)
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", format_outcome_stats(&r));
            }
        }
        StatsCommand::Reprice(a) => {
            let params = StatsRepriceParams {
                model: a.model.clone(),
                since: a.since.clone(),
                until: a.until.clone(),
                session_id: a.session.clone(),
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<StatsReprice>(params).await?) },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                let unpriced = if r.unpriced > 0 {
                    format!(" · {} unpriced (no [pricing] entry)", r.unpriced)
                } else {
                    String::new()
                };
                println!("repriced {} of {} calls{unpriced}", r.changed, r.examined);
            }
        }
    }
    Ok(())
}

/// Human output of `stats outcomes`: the range, the labelled share
/// against the dogfooding target, then the counts per kind.
pub fn format_outcome_stats(s: &OutcomeStats) -> String {
    let mut out = String::new();
    let since = s.range.since.as_deref().unwrap_or("beginning");
    let until = s.range.until.as_deref().unwrap_or("now");
    let _ = writeln!(out, "range {since} → {until}");
    let _ = writeln!(
        out,
        "runs {} · labelled {} ({:.0}%; target ≥ 60%)",
        s.agents,
        s.labelled,
        s.labelled_share * 100.0
    );
    let _ = writeln!(
        out,
        "tests passed {} · failed {} · accepted {} · rejected {} · done {}",
        s.tests_passed, s.tests_failed, s.accepted, s.rejected, s.done
    );
    if !s.by_kind.is_empty() {
        let _ = writeln!(
            out,
            "by kind: {}",
            s.by_kind
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    if !s.errors.is_empty() {
        let _ = writeln!(
            out,
            "errors: {}",
            s.errors
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    out
}

/// Human output: a range line, one table per requested breakdown, and the
/// one-line total.
pub fn format_token_stats(s: &TokenStats, by: &[By]) -> String {
    let mut out = String::new();
    let since = s.range.since.as_deref().unwrap_or("beginning");
    let until = s.range.until.as_deref().unwrap_or("now");
    let _ = writeln!(out, "range {since} → {until} (days in {})", s.tz);
    for group in by {
        let (title, rows) = match group {
            By::Model => ("model", &s.by_model),
            By::Day => ("day", &s.by_day),
            By::Session => ("session", &s.by_session),
            By::Workspace => ("workspace", &s.by_workspace),
            By::Kind => ("kind", &s.by_kind),
        };
        if *group == By::Session && s.by_session.is_empty() && s.totals.calls > 0 {
            // A single-session query leaves the session table empty.
            continue;
        }
        out.push('\n');
        out.push_str(&table(title, rows));
    }
    out.push('\n');
    out.push_str(&total_line(&s.totals));
    out.push('\n');
    out
}

/// `14 calls · in 182,340 · out 21,004 · cache read 610,222 · $1.84`.
pub fn total_line(b: &TokenBucket) -> String {
    let mut line = format!(
        "{} calls · in {} · out {} · cache read {}",
        thousands(b.calls),
        thousands(b.input),
        thousands(b.output),
        thousands(b.cache_read)
    );
    if b.cache_creation > 0 {
        let _ = write!(line, " · cache write {}", thousands(b.cache_creation));
    }
    let _ = write!(line, " · {}", usd(b.cost_usd));
    if b.unpriced_calls > 0 {
        let _ = write!(line, " ({} unpriced)", b.unpriced_calls);
    }
    line
}

fn table(title: &str, rows: &[TokenBucket]) -> String {
    let header = [title, "calls", "in", "out", "cache rd", "cache wr", "cost"];
    let mut cells: Vec<[String; 7]> = rows
        .iter()
        .map(|b| {
            let mut key = b.key.clone().unwrap_or_default();
            if let Some(label) = &b.label {
                key = format!("{key} ({label})");
            }
            let mut cost = usd(b.cost_usd);
            if b.unpriced_calls > 0 {
                cost.push('*');
            }
            [
                key,
                thousands(b.calls),
                thousands(b.input),
                thousands(b.output),
                thousands(b.cache_read),
                thousands(b.cache_creation),
                cost,
            ]
        })
        .collect();
    if cells.is_empty() {
        cells.push([
            "(none)".into(),
            "0".into(),
            "0".into(),
            "0".into(),
            "0".into(),
            "0".into(),
            usd(0.0),
        ]);
    }
    let widths: Vec<usize> = (0..7)
        .map(|i| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .chain(std::iter::once(header[i].chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |row: &[&str]| -> String {
        let mut s = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            if i == 0 {
                let _ = write!(s, "{cell:<w$}", w = widths[i]);
            } else {
                let _ = write!(s, "{cell:>w$}", w = widths[i]);
            }
        }
        s.trim_end().to_owned()
    };
    let mut out = line(&header);
    out.push('\n');
    for r in &cells {
        let refs: Vec<&str> = r.iter().map(String::as_str).collect();
        out.push_str(&line(&refs));
        out.push('\n');
    }
    if rows.iter().any(|b| b.unpriced_calls > 0) {
        out.push_str("* excludes calls without a [pricing] entry\n");
    }
    out
}

/// The columns of `stats calls --csv` and of the GUI's export, in order.
pub const CALL_CSV_COLUMNS: &[&str] = &[
    "call_id",
    "started_at",
    "session_id",
    "session_title",
    "workspace_id",
    "agent_id",
    "kind",
    "model",
    "effort",
    "status",
    "stop_reason",
    "input",
    "output",
    "cache_read",
    "cache_creation",
    "cost_usd",
    "total_ms",
    "request_event_id",
];

/// RFC 4180 rows, `\r\n` terminated, with a header line; absent values
/// are empty fields and the cost has its full precision.
pub fn calls_csv(calls: &[CallSummary]) -> String {
    let mut out = String::new();
    out.push_str(&CALL_CSV_COLUMNS.join(","));
    out.push_str("\r\n");
    for c in calls {
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        let fields = [
            c.call_id.clone(),
            c.started_at.clone(),
            c.session_id.clone(),
            opt(&c.session_title),
            opt(&c.workspace_id),
            c.agent_id.clone(),
            c.kind.clone(),
            c.model.clone(),
            opt(&c.effort),
            c.status.clone(),
            opt(&c.stop_reason),
            c.input.to_string(),
            c.output.to_string(),
            c.cache_read.to_string(),
            c.cache_creation.to_string(),
            c.cost_usd.map(|v| v.to_string()).unwrap_or_default(),
            c.total_ms.map(|v| v.to_string()).unwrap_or_default(),
            c.request_event_id.clone(),
        ];
        let row: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Human output of `stats calls`: one row per call and a count line.
pub fn format_calls(r: &StatsCallsResult, offset: u64) -> String {
    let header = [
        "started", "session", "kind", "model", "status", "in", "out", "cache rd", "cost", "ms",
    ];
    let mut cells: Vec<Vec<String>> = r
        .calls
        .iter()
        .map(|c| {
            let session = c
                .session_title
                .clone()
                .unwrap_or_else(|| c.session_id.clone());
            vec![
                c.started_at.clone(),
                session,
                c.kind.clone(),
                c.model.clone(),
                c.status.clone(),
                thousands(c.input),
                thousands(c.output),
                thousands(c.cache_read),
                c.cost_usd.map_or_else(|| "-".into(), usd),
                c.total_ms.map_or_else(|| "-".into(), thousands),
            ]
        })
        .collect();
    if cells.is_empty() {
        cells.push(
            std::iter::once("(none)".to_owned())
                .chain(std::iter::repeat_n(String::new(), header.len() - 1))
                .collect(),
        );
    }
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .chain(std::iter::once(header[i].chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let numeric = |i: usize| i >= 5;
    let line = |row: &[&str]| -> String {
        let mut s = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            if numeric(i) {
                let _ = write!(s, "{cell:>w$}", w = widths[i]);
            } else {
                let _ = write!(s, "{cell:<w$}", w = widths[i]);
            }
        }
        s.trim_end().to_owned()
    };
    let mut out = line(&header);
    out.push('\n');
    for r in &cells {
        let refs: Vec<&str> = r.iter().map(String::as_str).collect();
        out.push_str(&line(&refs));
        out.push('\n');
    }
    let shown = r.calls.len() as u64;
    if shown == r.total {
        let _ = writeln!(out, "{} calls", thousands(r.total));
    } else {
        let _ = writeln!(
            out,
            "calls {}–{} of {}",
            thousands(offset + 1),
            thousands(offset + shown),
            thousands(r.total)
        );
    }
    out
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Dollars with cents, or four decimals below a dollar so small calls are
/// not shown as `$0.00`.
pub fn usd(v: f64) -> String {
    if v != 0.0 && v.abs() < 1.0 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

#[cfg(test)]
mod outcome_tests {
    use apprentice_api::types::{OutcomeStats, StatsRange};

    use super::format_outcome_stats;

    #[test]
    fn outcome_stats_print_the_labelled_share_against_the_target() {
        let s = OutcomeStats {
            range: StatsRange {
                since: Some("2026-09-06T00:00:00Z".into()),
                until: None,
            },
            agents: 20,
            labelled: 13,
            labelled_share: 0.65,
            by_kind: [("files_changed".to_owned(), 17), ("tests".to_owned(), 11)]
                .into_iter()
                .collect(),
            tests_passed: 9,
            tests_failed: 2,
            accepted: 6,
            rejected: 1,
            done: 4,
            errors: [("stalled".to_owned(), 1)].into_iter().collect(),
        };
        assert_eq!(
            format_outcome_stats(&s),
            [
                "range 2026-09-06T00:00:00Z → now",
                "runs 20 · labelled 13 (65%; target ≥ 60%)",
                "tests passed 9 · failed 2 · accepted 6 · rejected 1 · done 4",
                "by kind: files_changed 17 · tests 11",
                "errors: stalled 1",
                "",
            ]
            .join(
                "
"
            )
        );
        let empty = OutcomeStats::default();
        assert!(format_outcome_stats(&empty).contains("runs 0 · labelled 0 (0%"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprentice_api::types::{ApprenticeStats, StatsRange};

    fn bucket(key: Option<&str>, label: Option<&str>, calls: u64, cost: f64) -> TokenBucket {
        TokenBucket {
            key: key.map(str::to_owned),
            label: label.map(str::to_owned),
            calls,
            input: 182_340,
            output: 21_004,
            cache_read: 610_222,
            cache_creation: 0,
            cost_usd: cost,
            unpriced_calls: 0,
        }
    }

    #[test]
    fn thousands_and_dollars() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(1.84), "$1.84");
        assert_eq!(usd(0.0138), "$0.0138");
        assert_eq!(usd(12.5), "$12.50");
    }

    #[test]
    fn total_line_matches_the_documented_shape() {
        assert_eq!(
            total_line(&bucket(None, None, 14, 1.84)),
            "14 calls · in 182,340 · out 21,004 · cache read 610,222 · $1.84"
        );
        let mut b = bucket(None, None, 14, 1.84);
        b.cache_creation = 12;
        b.unpriced_calls = 2;
        assert_eq!(
            total_line(&b),
            "14 calls · in 182,340 · out 21,004 · cache read 610,222 · cache write 12 · $1.84 (2 unpriced)"
        );
    }

    #[test]
    fn tables_align_and_label_sessions() {
        let s = TokenStats {
            range: StatsRange {
                since: Some("2026-09-05T10:30:00.000Z".into()),
                until: None,
            },
            tz: "+02:00".into(),
            totals: bucket(None, None, 14, 1.84),
            by_model: vec![bucket(Some("claude-opus-5"), None, 14, 1.84)],
            by_day: vec![],
            by_session: vec![bucket(Some("0192"), Some("fix tests"), 14, 1.84)],
            by_workspace: vec![],
            by_kind: vec![],
            apprentice: ApprenticeStats::default(),
        };
        let text = format_token_stats(&s, &[By::Model, By::Session, By::Day]);
        assert_eq!(
            text,
            "range 2026-09-05T10:30:00.000Z → now (days in +02:00)\n\
             \n\
             model          calls       in     out  cache rd  cache wr   cost\n\
             claude-opus-5     14  182,340  21,004   610,222         0  $1.84\n\
             \n\
             session           calls       in     out  cache rd  cache wr   cost\n\
             0192 (fix tests)     14  182,340  21,004   610,222         0  $1.84\n\
             \n\
             day     calls  in  out  cache rd  cache wr   cost\n\
             (none)      0   0    0         0         0  $0.00\n\
             \n\
             14 calls · in 182,340 · out 21,004 · cache read 610,222 · $1.84\n"
        );
    }

    #[test]
    fn single_session_queries_skip_the_empty_session_table() {
        let s = TokenStats {
            range: StatsRange::default(),
            tz: "+00:00".into(),
            totals: bucket(None, None, 1, 0.01),
            by_model: vec![],
            by_day: vec![],
            by_session: vec![],
            by_workspace: vec![],
            by_kind: vec![],
            apprentice: ApprenticeStats::default(),
        };
        let text = format_token_stats(&s, &[By::Session]);
        assert!(!text.contains("session"), "{text}");
        assert!(text.ends_with("$0.0100\n"), "{text}");
    }

    fn call(id: &str, kind: &str, title: Option<&str>, cost: Option<f64>) -> CallSummary {
        CallSummary {
            call_id: id.into(),
            started_at: "2026-09-12T10:04:00.000Z".into(),
            session_id: "s1".into(),
            session_title: title.map(str::to_owned),
            workspace_id: Some("w1".into()),
            agent_id: "a1".into(),
            kind: kind.into(),
            model: "claude-opus-5".into(),
            effort: Some("high".into()),
            status: "ok".into(),
            stop_reason: Some("end_turn".into()),
            input: 1204,
            output: 310,
            cache_read: 0,
            cache_creation: 900,
            cost_usd: cost,
            total_ms: Some(4200),
            request_event_id: "e7".into(),
        }
    }

    #[test]
    fn calls_table_has_a_count_line_and_pages() {
        let r = StatsCallsResult {
            calls: vec![
                call("m2", "title", Some("fix tests"), Some(0.0138)),
                call("m1", "step", None, None),
            ],
            total: 2,
        };
        assert_eq!(
            format_calls(&r, 0),
            "started                   session    kind   model          status     in  out  cache rd     cost     ms\n\
             2026-09-12T10:04:00.000Z  fix tests  title  claude-opus-5  ok      1,204  310         0  $0.0138  4,200\n\
             2026-09-12T10:04:00.000Z  s1         step   claude-opus-5  ok      1,204  310         0        -  4,200\n\
             2 calls\n"
        );
        let paged = StatsCallsResult { total: 120, ..r };
        assert!(format_calls(&paged, 100).ends_with("calls 101–102 of 120\n"));
        let none = StatsCallsResult {
            calls: vec![],
            total: 0,
        };
        assert!(format_calls(&none, 0).ends_with("(none)\n0 calls\n"));
    }

    #[test]
    fn csv_quotes_what_needs_it_and_leaves_absent_values_empty() {
        let mut quoted = call("m1", "step", Some("say \"hi\", twice"), Some(0.0138));
        quoted.stop_reason = None;
        quoted.effort = None;
        let text = calls_csv(&[quoted]);
        let mut lines = text.split("\r\n");
        assert_eq!(lines.next().unwrap(), CALL_CSV_COLUMNS.join(","));
        assert_eq!(
            lines.next().unwrap(),
            "m1,2026-09-12T10:04:00.000Z,s1,\"say \"\"hi\"\", twice\",w1,a1,step,claude-opus-5,,ok,,1204,310,0,900,0.0138,4200,e7"
        );
        assert_eq!(lines.next(), Some(""));
        assert_eq!(CALL_CSV_COLUMNS.len(), 18);
    }
}
