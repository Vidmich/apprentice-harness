//! `harness stats tokens | reprice` (task M00-07). Parsing of `--since` /
//! `--until` happens in the daemon so the CLI and GUI agree on it; the
//! values are passed through verbatim.

use std::fmt::Write as _;

use apprentice_api::methods::{StatsReprice, StatsRepriceParams, StatsTokens, StatsTokensParams};
use apprentice_api::types::{TokenBucket, TokenStats};
use clap::{Args, Subcommand, ValueEnum};

use crate::Ctx;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum StatsCommand {
    /// Mentor token usage and cost, aggregated from the trace store.
    Tokens(TokensArgs),
    /// Recompute stored call costs from the current `[pricing]` config.
    Reprice(RepriceArgs),
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
    /// Breakdown table(s) to print; repeatable. Default: model.
    #[arg(long, value_enum, value_name = "GROUP")]
    by: Vec<By>,
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
}

pub fn run(ctx: &Ctx, cmd: &StatsCommand) -> anyhow::Result<()> {
    let json = ctx.out.json;
    match cmd {
        StatsCommand::Tokens(a) => {
            let params = StatsTokensParams {
                since: a.since.clone(),
                until: a.until.clone(),
                session_id: a.session.clone(),
            };
            let stats = with_client(
                ctx,
                |c| async move { Ok(c.call::<StatsTokens>(params).await?) },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                let by = if a.by.is_empty() {
                    &[By::Model][..]
                } else {
                    &a.by
                };
                print!("{}", format_token_stats(&stats, by));
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

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
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
            apprentice: ApprenticeStats::default(),
        };
        let text = format_token_stats(&s, &[By::Session]);
        assert!(!text.contains("session"), "{text}");
        assert!(text.ends_with("$0.0100\n"), "{text}");
    }
}
