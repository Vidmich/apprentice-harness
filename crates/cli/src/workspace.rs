//! `harness workspace add | list | remove | info | refresh` (task M01-02).

use std::path::PathBuf;

use apprentice_api::methods::{
    Empty, WorkspaceAdd, WorkspaceAddParams, WorkspaceIdParams, WorkspaceInfo, WorkspaceInfoResult,
    WorkspaceList, WorkspaceRefresh, WorkspaceRemove,
};
use apprentice_client::DaemonClient;
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::config::workspace_string;
use crate::daemon::with_client;
use crate::session::short_time;

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Register a root directory (default: the current directory). The
    /// same root always gets the same id.
    Add(AddArgs),
    /// List registered workspaces, most recently used first.
    List,
    /// Forget a workspace. Files and traces are untouched.
    Remove(IdArgs),
    /// Root, file count, git head and `.harness/` facts of a workspace.
    Info(IdOrDirArgs),
    /// Re-read `.harness/ignore` and rebuild the file index.
    Refresh(IdOrDirArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// The root directory.
    #[arg(value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Display name (default: the directory name).
    #[arg(long, value_name = "TEXT")]
    name: Option<String>,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    /// Workspace id from `workspace list`.
    #[arg(value_name = "ID")]
    id: String,
}

#[derive(Debug, Args)]
pub struct IdOrDirArgs {
    /// Workspace id from `workspace list`, or a directory (registered
    /// on the way). Default: the current directory.
    #[arg(value_name = "ID_OR_DIR")]
    target: Option<String>,
}

pub fn run(ctx: &Ctx, cmd: &WorkspaceCommand) -> anyhow::Result<()> {
    match cmd {
        WorkspaceCommand::Add(a) => {
            let params = WorkspaceAddParams {
                root: workspace_string(&a.dir.clone().unwrap_or_else(|| PathBuf::from(".")))?,
                name: a.name.clone(),
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<WorkspaceAdd>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.line(&r.id);
                ctx.out.info(format!("{} at {}", r.name, r.root));
            }
        }
        WorkspaceCommand::List => {
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<WorkspaceList>(Empty::default()).await?)
            })?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else if r.workspaces.is_empty() {
                ctx.out.info("no workspaces registered");
            } else {
                let rows: Vec<Vec<String>> = r
                    .workspaces
                    .iter()
                    .map(|w| {
                        vec![
                            w.id.clone(),
                            short_time(&w.last_used_at),
                            w.name.clone(),
                            w.root.clone(),
                        ]
                    })
                    .collect();
                ctx.out
                    .print_table(&["id", "last used", "name", "root"], &rows);
            }
        }
        WorkspaceCommand::Remove(a) => {
            let params = WorkspaceIdParams { id: a.id.clone() };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<WorkspaceRemove>(params).await?)
            })?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.info(format!(
                    "removed {} ({} session(s) unlinked)",
                    a.id, r.sessions_unlinked
                ));
            }
        }
        WorkspaceCommand::Info(a) => {
            let target = target(a)?;
            let r = with_client(ctx, |c| async move {
                let id = resolve_id(&c, target).await?;
                Ok(c.call::<WorkspaceInfo>(WorkspaceIdParams { id }).await?)
            })?;
            print_info(ctx, &r)?;
        }
        WorkspaceCommand::Refresh(a) => {
            let target = target(a)?;
            let r = with_client(ctx, |c| async move {
                let id = resolve_id(&c, target).await?;
                Ok(c.call::<WorkspaceRefresh>(WorkspaceIdParams { id }).await?)
            })?;
            print_info(ctx, &r)?;
        }
    }
    Ok(())
}

/// An id, or a directory to register first.
#[derive(Debug)]
enum Target {
    Id(String),
    Dir(String),
}

fn target(a: &IdOrDirArgs) -> anyhow::Result<Target> {
    Ok(match &a.target {
        None => Target::Dir(workspace_string(&PathBuf::from("."))?),
        Some(t) if PathBuf::from(t).is_dir() => Target::Dir(workspace_string(&PathBuf::from(t))?),
        Some(t) => Target::Id(t.clone()),
    })
}

async fn resolve_id(c: &DaemonClient, target: Target) -> anyhow::Result<String> {
    Ok(match target {
        Target::Id(id) => id,
        Target::Dir(root) => {
            c.call::<WorkspaceAdd>(WorkspaceAddParams { root, name: None })
                .await?
                .id
        }
    })
}

fn print_info(ctx: &Ctx, r: &WorkspaceInfoResult) -> anyhow::Result<()> {
    if ctx.out.json {
        return ctx.out.emit_json(r);
    }
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let mut rows = vec![
        ("id", r.id.clone()),
        ("name", r.name.clone()),
        ("root", r.root.clone()),
        ("created", short_time(&r.created_at)),
        ("last used", short_time(&r.last_used_at)),
        (
            "files",
            if r.index_truncated {
                format!("{}+ (index truncated)", r.file_count)
            } else {
                r.file_count.to_string()
            },
        ),
        ("index age", format!("{} s", r.index_age_s)),
    ];
    let dirty = match r.git_dirty {
        Some(true) => ", dirty",
        Some(false) => ", clean",
        None => "",
    };
    if let Some(head) = &r.git_head {
        let short: String = head.chars().take(12).collect();
        rows.push((
            "git",
            match &r.git_branch {
                Some(b) => format!("{b} @ {short}{dirty}"),
                None => format!("detached @ {short}{dirty}"),
            },
        ));
    } else if let Some(b) = &r.git_branch {
        rows.push(("git", format!("{b} (no commits){dirty}")));
    }
    rows.push(("HARNESS.md", yes_no(r.has_instructions).to_owned()));
    rows.push(("config.toml", yes_no(r.has_config).to_owned()));
    rows.push(("ignore", yes_no(r.has_ignore_file).to_owned()));
    if !r.config_overrides.is_empty() {
        rows.push(("overrides", r.config_overrides.join(", ")));
    }
    for (k, v) in rows {
        ctx.out.line(format!("{k:<12} {v}"));
    }
    Ok(())
}
