//! `harness tools list` (task M01-01) and `tools rules|allow|deny` (task
//! M01-07): the permission rules in force and how to add one.

use std::path::PathBuf;

use apprentice_api::events::Risk;
use apprentice_api::methods::{
    ToolsAllow, ToolsDeny, ToolsList, ToolsListParams, ToolsRuleParams, ToolsRuleResult,
    ToolsRules, ToolsRulesParams,
};
use apprentice_api::types::{ConfigLayer, RuleEffect, RuleMatch, RuleSource, RuleSpec};
use clap::{Args, Subcommand, ValueEnum};

use crate::Ctx;
use crate::config::workspace_string;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum ToolsCommand {
    /// List the tools the daemon offers the mentor and whether each is
    /// enabled for a workspace.
    List(ListArgs),
    /// Show the permission rules in force: the workspace file, the user
    /// file, the built-ins.
    Rules(RulesArgs),
    /// Add an allow rule to a permissions file.
    Allow(RuleArgs),
    /// Add a deny rule to a permissions file.
    Deny(RuleArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Resolve `tools.disabled` for this workspace (default: the user
    /// config alone).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Show each tool's description too.
    #[arg(long)]
    describe: bool,
}

#[derive(Debug, Args)]
pub struct RulesArgs {
    /// The workspace whose `.harness/permissions.toml` to include
    /// (default: the current directory).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Only the user file and the built-ins.
    #[arg(long, conflicts_with = "workspace")]
    user: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  harness tools allow shell --command-prefix \"cargo test\"
  harness tools allow write_file --path \"docs/**\" --user
  harness tools deny shell --command-regex \"\\\\bgit\\\\s+push\\\\b\" --user
  harness tools deny read_file --outside --user")]
pub struct RuleArgs {
    /// Tool name, or `*` for every tool.
    tool: String,
    /// Glob on the root-relative path the call names (`src/**`).
    #[arg(long, value_name = "GLOB")]
    path: Option<String>,
    /// The command is this, or starts with this followed by a space.
    #[arg(long, value_name = "PREFIX")]
    command_prefix: Option<String>,
    /// A regular expression found anywhere in the command.
    #[arg(long, value_name = "REGEX")]
    command_regex: Option<String>,
    /// Only calls naming a path outside the workspace.
    #[arg(long, conflicts_with = "inside")]
    outside: bool,
    /// Only calls naming no path outside the workspace.
    #[arg(long)]
    inside: bool,
    /// Only tools of this risk class.
    #[arg(long, value_enum, value_name = "RISK")]
    risk: Option<RiskArg>,
    /// Write to this workspace's `.harness/permissions.toml` (default:
    /// the current directory).
    #[arg(long, value_name = "DIR", conflicts_with = "user")]
    workspace: Option<PathBuf>,
    /// Write to the user's `permissions.toml` instead.
    #[arg(long)]
    user: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RiskArg {
    ReadOnly,
    Write,
    Execute,
    Network,
}

impl From<RiskArg> for Risk {
    fn from(r: RiskArg) -> Self {
        match r {
            RiskArg::ReadOnly => Risk::ReadOnly,
            RiskArg::Write => Risk::Write,
            RiskArg::Execute => Risk::Execute,
            RiskArg::Network => Risk::Network,
        }
    }
}

impl RuleArgs {
    fn params(&self) -> anyhow::Result<ToolsRuleParams> {
        let (layer, workspace) = if self.user {
            (ConfigLayer::User, None)
        } else {
            let dir = self.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
            (ConfigLayer::Workspace, Some(workspace_string(&dir)?))
        };
        Ok(ToolsRuleParams {
            tool: self.tool.clone(),
            r#match: RuleMatch {
                path: self.path.clone(),
                command_prefix: self.command_prefix.clone(),
                command_regex: self.command_regex.clone(),
                outside_workspace: if self.outside {
                    Some(true)
                } else if self.inside {
                    Some(false)
                } else {
                    None
                },
                risk: self.risk.map(Risk::from),
            },
            layer,
            workspace,
        })
    }
}

pub fn run(ctx: &Ctx, cmd: &ToolsCommand) -> anyhow::Result<()> {
    match cmd {
        ToolsCommand::List(a) => list(ctx, a),
        ToolsCommand::Rules(a) => rules(ctx, a),
        ToolsCommand::Allow(a) => add_rule(ctx, a, RuleEffect::Allow),
        ToolsCommand::Deny(a) => add_rule(ctx, a, RuleEffect::Deny),
    }
}

fn list(ctx: &Ctx, a: &ListArgs) -> anyhow::Result<()> {
    let params = ToolsListParams {
        workspace: a.workspace.as_ref().map(workspace_string).transpose()?,
    };
    let r = with_client(
        ctx,
        |c| async move { Ok(c.call::<ToolsList>(params).await?) },
    )?;
    if ctx.out.json {
        ctx.out.emit_json(&r)?;
    } else if r.tools.is_empty() {
        ctx.out.info("no tools registered");
    } else {
        let rows: Vec<Vec<String>> = r
            .tools
            .iter()
            .map(|t| {
                let mut row = vec![
                    t.name.clone(),
                    risk_name(t.risk).to_owned(),
                    if t.enabled { "yes" } else { "no" }.to_owned(),
                    t.tags.join(","),
                ];
                if a.describe {
                    row.push(t.description.lines().next().unwrap_or("").to_owned());
                }
                row
            })
            .collect();
        let mut header = vec!["name", "risk", "enabled", "tags"];
        if a.describe {
            header.push("description");
        }
        ctx.out.print_table(&header, &rows);
    }
    Ok(())
}

fn rules(ctx: &Ctx, a: &RulesArgs) -> anyhow::Result<()> {
    let workspace = if a.user {
        None
    } else {
        let dir = a.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        Some(workspace_string(&dir)?)
    };
    let params = ToolsRulesParams { workspace };
    let r = with_client(
        ctx,
        |c| async move { Ok(c.call::<ToolsRules>(params).await?) },
    )?;
    if ctx.out.json {
        return ctx.out.emit_json(&r);
    }
    for f in &r.files {
        let state = match (&f.error, f.exists) {
            (Some(e), _) => format!("NOT IN FORCE: {e}"),
            (None, false) => "no file".to_owned(),
            (None, true) => match f.default {
                Some(d) => format!("default = {}", default_name(d)),
                None => "default = ask".to_owned(),
            },
        };
        ctx.out
            .line(format!("{:<9} {} ({state})", source_name(f.source), f.path));
    }
    ctx.out.line("");
    let rows: Vec<Vec<String>> = r
        .rules
        .iter()
        .map(|i| {
            let at = match (i.line, &i.name) {
                (Some(line), _) => format!("{}:{} (line {line})", source_name(i.source), i.index),
                (None, Some(name)) => format!("builtin:{name}"),
                (None, None) => format!("{}:{}", source_name(i.source), i.index),
            };
            vec![
                at,
                effect_name(i.rule.effect).to_owned(),
                i.rule.tool.clone(),
                describe_match(&i.rule.r#match),
            ]
        })
        .collect();
    ctx.out
        .print_table(&["rule", "effect", "tool", "match"], &rows);
    Ok(())
}

fn add_rule(ctx: &Ctx, a: &RuleArgs, effect: RuleEffect) -> anyhow::Result<()> {
    let params = a.params()?;
    let r: ToolsRuleResult = with_client(ctx, |c| async move {
        Ok(match effect {
            RuleEffect::Deny => c.call::<ToolsDeny>(params).await?,
            _ => c.call::<ToolsAllow>(params).await?,
        })
    })?;
    if ctx.out.json {
        return ctx.out.emit_json(&r);
    }
    let every = if r.rule.r#match.is_empty() {
        " (every call of the tool)"
    } else {
        ""
    };
    ctx.out.line(format!(
        "{}:{} {}{every}",
        r.path,
        r.line,
        describe_rule(&r.rule)
    ));
    Ok(())
}

/// `allow shell [command_prefix="cargo test"]`.
pub fn describe_rule(rule: &RuleSpec) -> String {
    let m = describe_match(&rule.r#match);
    if m.is_empty() {
        format!("{} {}", effect_name(rule.effect), rule.tool)
    } else {
        format!("{} {} [{m}]", effect_name(rule.effect), rule.tool)
    }
}

fn describe_match(m: &RuleMatch) -> String {
    let mut parts = Vec::new();
    if let Some(r) = m.risk {
        parts.push(format!("risk={}", risk_name(r)));
    }
    if let Some(p) = &m.path {
        parts.push(format!("path={p}"));
    }
    if let Some(p) = &m.command_prefix {
        parts.push(format!("command_prefix={p:?}"));
    }
    if let Some(r) = &m.command_regex {
        parts.push(format!("command_regex={r:?}"));
    }
    if let Some(o) = m.outside_workspace {
        parts.push(format!("outside_workspace={o}"));
    }
    parts.join(", ")
}

fn effect_name(e: RuleEffect) -> &'static str {
    match e {
        RuleEffect::Allow => "allow",
        RuleEffect::Deny => "deny",
        RuleEffect::Ask | _ => "ask",
    }
}

fn source_name(s: RuleSource) -> &'static str {
    match s {
        RuleSource::Workspace => "workspace",
        RuleSource::User => "user",
        RuleSource::Builtin | _ => "builtin",
    }
}

fn default_name(d: apprentice_api::types::RuleDefault) -> &'static str {
    match d {
        apprentice_api::types::RuleDefault::Deny => "deny",
        _ => "ask",
    }
}

pub fn risk_name(risk: Risk) -> &'static str {
    match risk {
        Risk::ReadOnly => "read-only",
        Risk::Write => "write",
        Risk::Execute => "execute",
        Risk::Network => "network",
        _ => "other",
    }
}
