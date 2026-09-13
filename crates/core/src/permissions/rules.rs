//! Rule files (`permissions.toml`), their parsing and matching, and the
//! built-in rules below them.
//!
//! ```toml
//! default = "ask"                  # ask | deny, for calls no rule matches
//!
//! [[rule]]
//! tool = "shell"                   # exact tool name or "*"
//! effect = "allow"                 # allow | deny | ask
//! [rule.match]                     # every condition given must hold
//! command_prefix = "cargo test"
//! ```
//!
//! Within one file the first matching rule wins, in file order; the
//! layers are ranked by [`evaluate`]. A file that does not parse is
//! reported with its line and puts every call in `ask` (never `allow`)
//! until it is fixed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use apprentice_api::events::Risk;
use apprentice_api::types::{
    RuleDefault, RuleEffect, RuleFileInfo, RuleInfo, RuleMatch, RuleSource, RuleSpec,
};
use globset::{GlobBuilder, GlobMatcher};
use regex::Regex;
use serde::Deserialize;
use toml::Spanned;

use super::PermissionRequest;
use crate::tools::file::write_atomic;

/// A rules file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum RuleFileError {
    /// TOML or rule error at a line.
    #[error("{}:{line}: {message}", path.display())]
    Parse {
        path: PathBuf,
        line: u64,
        message: String,
    },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A rule ready to match.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub spec: RuleSpec,
    /// Line of the `[[rule]]` header, for file rules.
    pub line: Option<u64>,
    /// Built-in rules have a name.
    pub name: Option<String>,
    glob: Option<GlobMatcher>,
    regex: Option<Regex>,
}

impl CompiledRule {
    /// Compiles the glob and the regex.
    ///
    /// # Errors
    /// A message naming the bad field.
    pub fn compile(spec: RuleSpec) -> Result<Self, String> {
        if spec.tool.is_empty() {
            return Err("`tool` must be a tool name or `*`".to_owned());
        }
        let glob = spec
            .r#match
            .path
            .as_deref()
            .map(|p| {
                GlobBuilder::new(p)
                    .literal_separator(true)
                    .build()
                    .map(|g| g.compile_matcher())
                    .map_err(|e| format!("`match.path`: {e}"))
            })
            .transpose()?;
        let regex = spec
            .r#match
            .command_regex
            .as_deref()
            .map(|r| Regex::new(r).map_err(|e| format!("`match.command_regex`: {e}")))
            .transpose()?;
        Ok(Self {
            spec,
            line: None,
            name: None,
            glob,
            regex,
        })
    }

    fn named(spec: RuleSpec, name: &str) -> Self {
        let mut rule = Self::compile(spec).expect("built-in rule compiles");
        rule.name = Some(name.to_owned());
        rule
    }

    /// Whether every condition holds for `req`.
    pub fn matches(&self, req: &PermissionRequest) -> bool {
        let m = &self.spec.r#match;
        if self.spec.tool != "*" && self.spec.tool != req.tool {
            return false;
        }
        if let Some(risk) = m.risk
            && risk != req.risk
        {
            return false;
        }
        if let Some(outside) = m.outside_workspace
            && outside != req.has_outside()
        {
            return false;
        }
        // A glob covers outside paths only when the rule says so, or
        // `path = "**"` would reach past the root.
        if let Some(glob) = &self.glob
            && (req.paths.is_empty()
                || !req.paths.iter().all(|p| {
                    (!p.outside || m.outside_workspace == Some(true))
                        && path_matches(glob, &p.shown)
                }))
        {
            return false;
        }
        if let Some(prefix) = &m.command_prefix
            && !req
                .command
                .as_deref()
                .is_some_and(|c| command_has_prefix(c, prefix))
        {
            return false;
        }
        if let Some(regex) = &self.regex
            && !req.command.as_deref().is_some_and(|c| regex.is_match(c))
        {
            return false;
        }
        true
    }

    /// `tool [condition, ...]` for reasons and listings.
    pub fn describe(&self) -> String {
        let m = &self.spec.r#match;
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
        if parts.is_empty() {
            self.spec.tool.clone()
        } else {
            format!("{} [{}]", self.spec.tool, parts.join(", "))
        }
    }
}

pub(super) fn risk_name(risk: Risk) -> &'static str {
    match risk {
        Risk::ReadOnly => "read_only",
        Risk::Write => "write",
        Risk::Execute => "execute",
        Risk::Network => "network",
        _ => "other",
    }
}

/// `src/**` also covers `src` itself; the root (`.`) is covered by
/// `**` alone.
fn path_matches(glob: &GlobMatcher, shown: &str) -> bool {
    if glob.is_match(shown) {
        return true;
    }
    let pattern = glob.glob().glob();
    if let Some(dir) = pattern.strip_suffix("/**") {
        return shown == dir;
    }
    pattern == "**" && shown == "."
}

/// The prefix is the whole command or a leading run of its words. A
/// command chain (`;`, `&&`, `|`, a newline, command substitution) is
/// never matched by a prefix: it would let `cargo test; rm -rf .`
/// through an `allow cargo test` rule. Use `command_regex` for those.
pub fn command_has_prefix(command: &str, prefix: &str) -> bool {
    let command = command.trim();
    let prefix = prefix.trim();
    if prefix.is_empty() || is_chain(command) {
        return false;
    }
    match command.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with(char::is_whitespace),
        None => false,
    }
}

/// More than one command in one string.
pub fn is_chain(command: &str) -> bool {
    command.contains(['\n', ';', '&', '|', '`']) || command.contains("$(")
}

// ------------------------------------------------------------- built-ins

/// Commands no rule file needs to name: recursive deletion of a root,
/// the home or the working directory, disk formatting, a fork bomb,
/// recursive mode changes of `/`. An explicit `allow` in a file wins
/// over these.
const CATASTROPHIC: &[(&str, &str)] = &[
    (
        "rm_recursive_root",
        r#"(?i)(^|[;&|]\s*|\bsudo\s+)rm\s+(-[a-z-]+\s+)*(-[a-z]*r[a-z]*|--recursive)\s+(-[a-z-]+\s+)*['"]?(/\*?|~/?\*?|\$HOME/?\*?|%USERPROFILE%|\*|\.{1,2}/?\*?|[a-z]:[\\/]?\*?)['"]?(\s|;|&|\||$)"#,
    ),
    (
        "remove_item_recursive_root",
        r#"(?i)\bRemove-Item\b([^;|]*\s-Recurse\b[^;|]*?(\s|['"])|[^;|]*?(\s|['"]))(/|\\|[a-z]:[\\/]?|~|\$HOME|\$env:USERPROFILE|\.\.?|\*)['"]?(\s[^;|]*-Recurse\b|\s|$)"#,
    ),
    (
        "disk_format",
        r"(?i)(^|[;&|]\s*|\bsudo\s+)(mkfs(\.[a-z0-9]+)?\s|format(\.com)?\s+[a-z]:|diskpart(\s|$)|dd\s+[^;|]*of=/dev/)",
    ),
    ("fork_bomb", r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:"),
    (
        "chmod_recursive_root",
        r"(?i)(^|[;&|]\s*|\bsudo\s+)(chmod|chown)\s+(-[a-z]*R[a-z]*|--recursive)\s+\S+\s+/(\s|;|&|\||$)",
    ),
];

/// The rules below every file: read-only calls inside the workspace are
/// allowed, the [`CATASTROPHIC`] commands denied.
pub fn builtin_rules() -> &'static [CompiledRule] {
    static RULES: OnceLock<Vec<CompiledRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = vec![CompiledRule::named(
            RuleSpec {
                tool: "*".into(),
                effect: RuleEffect::Allow,
                r#match: RuleMatch {
                    risk: Some(Risk::ReadOnly),
                    outside_workspace: Some(false),
                    ..RuleMatch::default()
                },
            },
            "read_only_inside",
        )];
        for (name, regex) in CATASTROPHIC {
            rules.push(CompiledRule::named(
                RuleSpec {
                    tool: "shell".into(),
                    effect: RuleEffect::Deny,
                    r#match: RuleMatch {
                        command_regex: Some((*regex).to_owned()),
                        ..RuleMatch::default()
                    },
                },
                name,
            ));
        }
        rules
    })
}

// ----------------------------------------------------------------- files

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileModel {
    #[serde(default)]
    default: Option<Spanned<String>>,
    #[serde(default)]
    rule: Vec<Spanned<RuleSpec>>,
}

/// Parses `text` (the content of `path`, named in errors).
///
/// # Errors
/// TOML errors and bad globs or regexes, with the line.
pub fn parse_rules(
    path: &Path,
    text: &str,
) -> Result<(Option<RuleDefault>, Vec<CompiledRule>), RuleFileError> {
    let parse = |line, message| RuleFileError::Parse {
        path: path.to_path_buf(),
        line,
        message,
    };
    let model: FileModel = toml::from_str(text).map_err(|e| {
        let line = e.span().map_or(1, |s| line_of(text, s.start));
        parse(line, e.message().to_owned())
    })?;
    let default = match model.default {
        None => None,
        Some(d) => match d.get_ref().as_str() {
            "ask" => Some(RuleDefault::Ask),
            "deny" => Some(RuleDefault::Deny),
            other => {
                return Err(parse(
                    line_of(text, d.span().start),
                    format!("`default` must be \"ask\" or \"deny\", not {other:?}"),
                ));
            }
        },
    };
    let mut rules = Vec::with_capacity(model.rule.len());
    for spanned in model.rule {
        let line = line_of(text, spanned.span().start);
        let mut rule = CompiledRule::compile(spanned.into_inner()).map_err(|m| parse(line, m))?;
        rule.line = Some(line);
        rules.push(rule);
    }
    Ok((default, rules))
}

/// 1-based line of byte `offset`.
fn line_of(text: &str, offset: usize) -> u64 {
    let offset = offset.min(text.len());
    text[..offset].bytes().filter(|b| *b == b'\n').count() as u64 + 1
}

/// Appends `rule` to `path` (created when missing) with `comment`
/// above it; returns the line of the new `[[rule]]` header. The file
/// is re-parsed afterwards, so a rule that would break it is refused.
///
/// # Errors
/// The rule does not compile, the file does not parse, or I/O.
pub fn append_rule(path: &Path, rule: &RuleSpec, comment: &str) -> Result<u64, RuleFileError> {
    let io = |source| RuleFileError::Io {
        path: path.to_path_buf(),
        source,
    };
    CompiledRule::compile(rule.clone()).map_err(|m| RuleFileError::Parse {
        path: path.to_path_buf(),
        line: 0,
        message: m,
    })?;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io(e)),
    };
    // The existing content must be sound before anything is added.
    parse_rules(path, &text)?;
    let mut doc: toml_edit::DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| RuleFileError::Parse {
                path: path.to_path_buf(),
                line: e.span().map_or(1, |s| line_of(&text, s.start)),
                message: e.message().to_owned(),
            })?;
    let mut table = toml_edit::Table::new();
    if !comment.is_empty() {
        let mut prefix = String::new();
        if !text.trim().is_empty() {
            prefix.push('\n');
        }
        for line in comment.lines() {
            let _ = writeln!(prefix, "# {line}");
        }
        table.decor_mut().set_prefix(prefix);
    }
    table["tool"] = toml_edit::value(rule.tool.as_str());
    table["effect"] = toml_edit::value(effect_name(rule.effect));
    let m = &rule.r#match;
    if !m.is_empty() {
        let mut sub = toml_edit::Table::new();
        if let Some(r) = m.risk {
            sub["risk"] = toml_edit::value(risk_name(r));
        }
        if let Some(p) = &m.path {
            sub["path"] = toml_edit::value(p.as_str());
        }
        if let Some(p) = &m.command_prefix {
            sub["command_prefix"] = toml_edit::value(p.as_str());
        }
        if let Some(r) = &m.command_regex {
            sub["command_regex"] = toml_edit::value(r.as_str());
        }
        if let Some(o) = m.outside_workspace {
            sub["outside_workspace"] = toml_edit::value(o);
        }
        table["match"] = toml_edit::Item::Table(sub);
    }
    if !doc.contains_key("rule") {
        doc["rule"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let rules = doc["rule"]
        .as_array_of_tables_mut()
        .ok_or_else(|| RuleFileError::Parse {
            path: path.to_path_buf(),
            line: 1,
            message: "`rule` is not an array of tables".to_owned(),
        })?;
    rules.push(table);
    let text = doc.to_string();
    let (_, parsed) = parse_rules(path, &text)?;
    let line = parsed.last().and_then(|r| r.line).unwrap_or(1);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    write_atomic(path, text.as_bytes()).map_err(io)?;
    Ok(line)
}

pub(super) fn effect_name(effect: RuleEffect) -> &'static str {
    match effect {
        RuleEffect::Allow => "allow",
        RuleEffect::Deny => "deny",
        RuleEffect::Ask | _ => "ask",
    }
}

/// One rules file as loaded.
#[derive(Debug)]
pub struct Layer {
    pub source: RuleSource,
    pub path: PathBuf,
    pub state: LayerState,
    /// `(mtime, len)` of the file as last read; `None` when missing.
    stamp: Option<(SystemTime, u64)>,
}

#[derive(Debug)]
pub enum LayerState {
    /// No file: no rules, `default = "ask"`.
    Missing,
    Loaded {
        /// The file's own `default`, when it sets one.
        default: Option<RuleDefault>,
        rules: Vec<CompiledRule>,
    },
    /// The file exists but cannot be used; every call is asked.
    Broken(RuleFileError),
}

impl Layer {
    /// Reads `path` now.
    pub fn load(source: RuleSource, path: impl Into<PathBuf>) -> Self {
        let mut layer = Self {
            source,
            path: path.into(),
            state: LayerState::Missing,
            stamp: None,
        };
        layer.reload();
        layer
    }

    fn stamp_now(&self) -> Option<(SystemTime, u64)> {
        let meta = std::fs::metadata(&self.path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    pub fn reload(&mut self) {
        self.stamp = self.stamp_now();
        self.state = match std::fs::read_to_string(&self.path) {
            Ok(text) => match parse_rules(&self.path, &text) {
                Ok((default, rules)) => LayerState::Loaded { default, rules },
                Err(e) => {
                    tracing::warn!(file = %self.path.display(), error = %e, "permission rules not in force");
                    LayerState::Broken(e)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LayerState::Missing,
            Err(source) => LayerState::Broken(RuleFileError::Io {
                path: self.path.clone(),
                source,
            }),
        };
    }

    /// Re-reads the file when its size or mtime changed.
    pub fn reload_if_changed(&mut self) {
        if self.stamp_now() != self.stamp {
            self.reload();
        }
    }

    pub fn rules(&self) -> &[CompiledRule] {
        match &self.state {
            LayerState::Loaded { rules, .. } => rules,
            LayerState::Missing | LayerState::Broken(_) => &[],
        }
    }

    /// The file's explicit `default`.
    pub fn default(&self) -> Option<RuleDefault> {
        match &self.state {
            LayerState::Loaded { default, .. } => *default,
            LayerState::Missing | LayerState::Broken(_) => None,
        }
    }

    pub fn error(&self) -> Option<&RuleFileError> {
        match &self.state {
            LayerState::Broken(e) => Some(e),
            LayerState::Missing | LayerState::Loaded { .. } => None,
        }
    }

    /// The first rule matching `req`, with its 1-based index.
    pub fn first_match(&self, req: &PermissionRequest) -> Option<(usize, &CompiledRule)> {
        self.rules()
            .iter()
            .enumerate()
            .find(|(_, r)| r.matches(req))
            .map(|(i, r)| (i + 1, r))
    }

    pub fn info(&self) -> RuleFileInfo {
        RuleFileInfo {
            source: self.source,
            path: self.path.to_string_lossy().into_owned(),
            exists: !matches!(self.state, LayerState::Missing),
            default: self.default(),
            error: self.error().map(ToString::to_string),
        }
    }

    /// The rules as `tools.rules` lists them.
    pub fn list(&self) -> Vec<RuleInfo> {
        self.rules()
            .iter()
            .enumerate()
            .map(|(i, r)| RuleInfo {
                source: self.source,
                index: i + 1,
                line: r.line,
                name: r.name.clone(),
                rule: r.spec.clone(),
            })
            .collect()
    }
}

/// The built-ins as `tools.rules` lists them.
pub fn list_builtins() -> Vec<RuleInfo> {
    builtin_rules()
        .iter()
        .enumerate()
        .map(|(i, r)| RuleInfo {
            source: RuleSource::Builtin,
            index: i + 1,
            line: None,
            name: r.name.clone(),
            rule: r.spec.clone(),
        })
        .collect()
}

/// How the layers answer a request; `rule_ref` names the rule (or the
/// `default`) that decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub effect: RuleEffect,
    pub rule_ref: Option<String>,
    /// What the rule looks like, for reasons.
    pub rule: Option<String>,
    /// Files that are not in force (their error), which force `ask`.
    pub broken: Vec<String>,
}

fn source_name(source: RuleSource) -> &'static str {
    match source {
        RuleSource::Workspace => "workspace",
        RuleSource::User => "user",
        RuleSource::Builtin | _ => "builtin",
    }
}

/// Ranks the layers (given highest first: workspace, user): a deny
/// from any layer in that order; a broken file (ask); the first match
/// of any layer in that order; the built-ins; the explicit `default`
/// of the first layer that sets one; else `ask`.
pub fn evaluate(layers: &[&Layer], req: &PermissionRequest) -> Evaluation {
    let matches: Vec<(&Layer, Option<(usize, &CompiledRule)>)> =
        layers.iter().map(|l| (*l, l.first_match(req))).collect();
    let decided = |layer: &Layer, index: usize, rule: &CompiledRule| Evaluation {
        effect: rule.spec.effect,
        rule_ref: Some(format!("{}:{index}", source_name(layer.source))),
        rule: Some(rule.describe()),
        broken: Vec::new(),
    };
    for (layer, m) in &matches {
        if let Some((i, r)) = m
            && r.spec.effect == RuleEffect::Deny
        {
            return decided(layer, *i, r);
        }
    }
    let broken: Vec<String> = layers
        .iter()
        .filter_map(|l| l.error().map(ToString::to_string))
        .collect();
    if !broken.is_empty() {
        return Evaluation {
            effect: RuleEffect::Ask,
            rule_ref: None,
            rule: None,
            broken,
        };
    }
    for (layer, m) in &matches {
        if let Some((i, r)) = m {
            return decided(layer, *i, r);
        }
    }
    if let Some(r) = builtin_rules().iter().find(|r| r.matches(req)) {
        return Evaluation {
            effect: r.spec.effect,
            rule_ref: Some(format!("builtin:{}", r.name.as_deref().unwrap_or("?"))),
            rule: Some(r.describe()),
            broken: Vec::new(),
        };
    }
    for layer in layers {
        if let Some(default) = layer.default() {
            return Evaluation {
                effect: match default {
                    RuleDefault::Deny => RuleEffect::Deny,
                    RuleDefault::Ask | _ => RuleEffect::Ask,
                },
                rule_ref: Some(format!("default:{}", source_name(layer.source))),
                rule: None,
                broken: Vec::new(),
            };
        }
    }
    Evaluation {
        effect: RuleEffect::Ask,
        rule_ref: None,
        rule: None,
        broken: Vec::new(),
    }
}
