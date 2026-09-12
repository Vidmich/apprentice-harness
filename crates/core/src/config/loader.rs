//! Layered loading: built-in defaults → user file → workspace file →
//! environment overrides, with per-key provenance.
//!
//! Layers are merged as JSON trees so provenance can be tracked per leaf;
//! the merged tree is then deserialised into [`Config`] after every layer so
//! an invalid value is attributed to the file that introduced it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apprentice_api::types::ConfigSource;
use serde_json::Value;

use super::error::ConfigError;
use super::keypath;
use super::paths::Paths;
use super::schema::{Config, ENV_OVERRIDES, WORKSPACE_OVERRIDABLE, workspace_overridable};

/// Where a value came from (re-export of the wire enum).
pub type Source = ConfigSource;

/// Result of loading: the typed config, the merged tree and provenance.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub config: Config,
    /// Merged tree; `get` reads from it so tables can be returned too.
    pub tree: Value,
    /// Winning layer per dotted leaf key.
    pub sources: BTreeMap<String, Source>,
    /// Files that were read (present on disk).
    pub user_file: Option<PathBuf>,
    pub workspace_file: Option<PathBuf>,
}

impl Resolved {
    /// Value at a dotted key. Leaves report their source; tables report
    /// `None` because their fields may come from different layers.
    ///
    /// # Errors
    /// `BadKey` for a malformed key, `NoSuchKey` when absent.
    pub fn get(&self, key: &str) -> Result<(Value, Option<Source>), ConfigError> {
        let segments = keypath::parse(key)?;
        let mut node = &self.tree;
        for s in &segments {
            node = node.get(s).ok_or_else(|| ConfigError::NoSuchKey {
                key: key.to_owned(),
            })?;
        }
        let source = if node.is_object() {
            None
        } else {
            self.sources.get(&keypath::format(&segments)).copied()
        };
        Ok((node.clone(), source))
    }

    /// Source of a leaf key, if known.
    pub fn source(&self, key: &str) -> Option<Source> {
        self.sources.get(key).copied()
    }
}

/// Loads configuration for a [`Paths`] and an optional workspace.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    paths: Paths,
    env: BTreeMap<String, String>,
}

impl ConfigLoader {
    /// Loader without environment overrides (tests, or callers that pass
    /// them explicitly with [`Self::with_env`]).
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            env: BTreeMap::new(),
        }
    }

    /// Reads the supported override variables from the process environment.
    ///
    /// # Errors
    /// A set variable that is not valid UTF-8.
    pub fn with_process_env(mut self) -> Result<Self, ConfigError> {
        for (var, _) in ENV_OVERRIDES {
            match std::env::var(var) {
                Ok(v) => {
                    self.env.insert((*var).to_owned(), v);
                }
                Err(std::env::VarError::NotPresent) => {}
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(ConfigError::BadEnv {
                        var: (*var).to_owned(),
                    });
                }
            }
        }
        Ok(self)
    }

    /// Explicit environment overrides (variable name → value).
    #[must_use]
    pub fn with_env<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Loads and merges all layers.
    ///
    /// # Errors
    /// Any unreadable, malformed or invalid layer; see [`ConfigError`].
    pub fn load(&self, workspace: Option<&Path>) -> Result<Resolved, ConfigError> {
        let mut merged = Merged::defaults();

        let user_file = self.paths.config_file();
        let user_file = if user_file.is_file() {
            let text = read(&user_file)?;
            merged.apply_text(&text, &user_file, Source::User)?;
            Some(user_file)
        } else {
            None
        };

        let workspace_file = workspace
            .map(Paths::workspace_config_file)
            .filter(|p| p.is_file());
        if let Some(path) = &workspace_file {
            let text = read(path)?;
            merged.apply_text(&text, path, Source::Workspace)?;
        }

        for (var, key) in ENV_OVERRIDES {
            if let Some(value) = self.env.get(*var) {
                let path = PathBuf::from(format!("${var}"));
                let segments = keypath::parse(key)?;
                merged.set_leaf(&segments, Value::String(value.clone()), Source::Env);
                merged.validate(&path)?;
            }
        }

        Ok(Resolved {
            config: merged.config,
            tree: merged.tree,
            sources: merged.sources,
            user_file,
            workspace_file,
        })
    }
}

/// Validates one layer's text on its own (defaults + layer), as `set` does
/// before writing a file.
pub(super) fn validate_layer_text(
    text: &str,
    path: &Path,
    source: Source,
) -> Result<(), ConfigError> {
    Merged::defaults().apply_text(text, path, source)
}

/// Merge state while layering.
struct Merged {
    config: Config,
    defaults: Value,
    tree: Value,
    sources: BTreeMap<String, Source>,
}

impl Merged {
    fn defaults() -> Self {
        let config = Config::builtin();
        let tree = serde_json::to_value(&config).expect("config serialises");
        let mut sources = BTreeMap::new();
        for (segments, _) in leaves(&tree) {
            sources.insert(keypath::format(&segments), Source::Default);
        }
        Self {
            config,
            defaults: tree.clone(),
            tree,
            sources,
        }
    }

    fn apply_text(&mut self, text: &str, path: &Path, source: Source) -> Result<(), ConfigError> {
        let table: toml::Table = toml::from_str(text).map_err(|e| ConfigError::Syntax {
            path: path.to_path_buf(),
            message: e.message().to_owned(),
        })?;
        let layer = serde_json::to_value(table).expect("toml table serialises");
        for (segments, value) in leaves(&layer) {
            let key = keypath::format(&segments);
            self.check_known(&segments, &key, &value, path)?;
            if source == Source::Workspace && !workspace_overridable(&key) {
                return Err(ConfigError::KeyNotOverridable {
                    path: path.to_path_buf(),
                    key,
                    allowed: WORKSPACE_OVERRIDABLE.join(", "),
                });
            }
            self.set_leaf(&segments, value, source);
        }
        self.validate(path)
    }

    /// A leaf is known when the same path is a leaf in the defaults, or it
    /// is a field of a (possibly new) pricing entry.
    fn check_known(
        &self,
        segments: &[String],
        key: &str,
        value: &Value,
        path: &Path,
    ) -> Result<(), ConfigError> {
        let defaults = &self.defaults;
        let mut node = defaults;
        for (i, s) in segments.iter().enumerate() {
            let Some(next) = node.get(s) else {
                // pricing.<any model>.<field>
                if segments.len() == 3 && segments[0] == "pricing" && i == 1 {
                    let template = &defaults["pricing"]["claude-opus-5"];
                    return match template.get(&segments[2]) {
                        Some(expected) => check_type(expected, value, key, path),
                        None => Err(ConfigError::UnknownKey {
                            path: path.to_path_buf(),
                            key: key.to_owned(),
                        }),
                    };
                }
                return Err(ConfigError::UnknownKey {
                    path: path.to_path_buf(),
                    key: key.to_owned(),
                });
            };
            node = next;
        }
        if node.is_object() {
            // The layer supplies a scalar where the schema has a table
            // (an empty table in the layer produces no leaf, so it is fine).
            return Err(ConfigError::WrongType {
                path: path.to_path_buf(),
                key: key.to_owned(),
                expected: "table",
                found: type_name(value),
            });
        }
        check_type(node, value, key, path)
    }

    fn set_leaf(&mut self, segments: &[String], value: Value, source: Source) {
        let mut node = &mut self.tree;
        for s in &segments[..segments.len() - 1] {
            if !node.is_object() {
                *node = Value::Object(serde_json::Map::new());
            }
            node = node
                .as_object_mut()
                .expect("just ensured object")
                .entry(s.clone())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
        }
        if !node.is_object() {
            *node = Value::Object(serde_json::Map::new());
        }
        let last = segments.last().expect("non-empty key");
        node.as_object_mut()
            .expect("just ensured object")
            .insert(last.clone(), value);
        self.sources.insert(keypath::format(segments), source);
    }

    fn validate(&mut self, path: &Path) -> Result<(), ConfigError> {
        self.config =
            serde_json::from_value(self.tree.clone()).map_err(|e| ConfigError::Invalid {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(())
    }
}

fn check_type(expected: &Value, found: &Value, key: &str, path: &Path) -> Result<(), ConfigError> {
    let (e, f) = (type_name(expected), type_name(found));
    if e == f {
        Ok(())
    } else {
        Err(ConfigError::WrongType {
            path: path.to_path_buf(),
            key: key.to_owned(),
            expected: e,
            found: f,
        })
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "table",
    }
}

/// All leaf `(path, value)` pairs of a tree. Arrays are leaves; empty
/// tables produce nothing.
fn leaves(tree: &Value) -> Vec<(Vec<String>, Value)> {
    fn walk(node: &Value, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, Value)>) {
        match node {
            Value::Object(map) => {
                for (k, v) in map {
                    prefix.push(k.clone());
                    walk(v, prefix, out);
                    prefix.pop();
                }
            }
            other => out.push((prefix.clone(), other.clone())),
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut Vec::new(), &mut out);
    out
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })
}
