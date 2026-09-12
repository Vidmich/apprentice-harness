//! Writing a single key into a layer file while preserving comments and
//! formatting (`toml_edit`).

use std::path::{Path, PathBuf};

use apprentice_api::types::ConfigLayer;
use serde_json::Value;
use toml_edit::{DocumentMut, Item};

use super::error::ConfigError;
use super::keypath;
use super::loader::{Source, validate_layer_text};
use apprentice_common::paths::Paths;

/// Sets (or, with `Value::Null`, removes) a dotted key in the user or
/// workspace file. An object value replaces the whole sub-table (used for
/// `pricing.<model>`); a scalar never silently replaces an existing table.
/// The file is created if missing, validated before writing and written
/// atomically.
///
/// # Errors
/// Malformed key, unknown key, non-overridable workspace key, wrong type,
/// or I/O.
#[allow(clippy::needless_pass_by_value)] // `value` is consumed conceptually; keeps call sites simple
pub fn set(
    paths: &Paths,
    layer: ConfigLayer,
    workspace: Option<&Path>,
    key: &str,
    value: Value,
) -> Result<(), ConfigError> {
    let (path, source) = match layer {
        ConfigLayer::User => (paths.config_file(), Source::User),
        ConfigLayer::Workspace => {
            let ws = workspace.ok_or(ConfigError::WorkspaceRequired)?;
            (Paths::workspace_config_file(ws), Source::Workspace)
        }
        other => {
            return Err(ConfigError::Invalid {
                path: PathBuf::new(),
                message: format!("unsupported config layer {other:?}"),
            });
        }
    };
    let segments = keypath::parse(key)?;

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    let mut doc: DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Syntax {
                path: path.clone(),
                message: e.message().to_owned(),
            })?;

    match &value {
        Value::Null => remove(&mut doc, &segments),
        Value::Object(map) => insert(&mut doc, &segments, Item::Table(to_table(map, key)?), key)?,
        scalar => insert(
            &mut doc,
            &segments,
            toml_edit::value(to_toml(scalar, key)?),
            key,
        )?,
    }

    let new_text = doc.to_string();
    validate_layer_text(&new_text, &path, source)?;
    write_atomic(&path, &new_text)
}

fn insert(
    doc: &mut DocumentMut,
    segments: &[String],
    item: Item,
    key: &str,
) -> Result<(), ConfigError> {
    let mut table = doc.as_table_mut();
    for s in &segments[..segments.len() - 1] {
        let created = !table.contains_key(s);
        let item = table.entry(s).or_insert_with(toml_edit::table);
        let Some(next) = item.as_table_mut() else {
            return Err(ConfigError::NotALeaf {
                key: key.to_owned(),
            });
        };
        if created {
            // Do not emit an empty `[parent]` header for intermediate tables.
            next.set_implicit(true);
        }
        table = next;
    }
    let last = &segments[segments.len() - 1];
    let mut item = item;
    match (table.get(last), item.as_value_mut()) {
        (Some(existing), None) if existing.is_table() => {}
        (Some(existing), Some(_)) if existing.is_table() => {
            return Err(ConfigError::NotALeaf {
                key: key.to_owned(),
            });
        }
        // Replacing a value keeps its surrounding whitespace and trailing
        // comment.
        (Some(existing), Some(new)) => {
            if let Some(old) = existing.as_value() {
                *new.decor_mut() = old.decor().clone();
            }
        }
        _ => {}
    }
    table[last] = item;
    Ok(())
}

fn to_table(
    map: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<toml_edit::Table, ConfigError> {
    let mut table = toml_edit::Table::new();
    for (k, v) in map {
        let item = match v {
            Value::Object(inner) => Item::Table(to_table(inner, key)?),
            Value::Null => continue,
            scalar => toml_edit::value(to_toml(scalar, key)?),
        };
        table[k] = item;
    }
    Ok(table)
}

fn remove(doc: &mut DocumentMut, segments: &[String]) {
    let mut table = doc.as_table_mut();
    for s in &segments[..segments.len() - 1] {
        match table.get_mut(s).and_then(Item::as_table_mut) {
            Some(next) => table = next,
            None => return,
        }
    }
    table.remove(&segments[segments.len() - 1]);
}

fn to_toml(value: &Value, key: &str) -> Result<toml_edit::Value, ConfigError> {
    Ok(match value {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else if let Some(f) = n.as_f64() {
                f.into()
            } else {
                return Err(ConfigError::BadKey {
                    key: key.to_owned(),
                    reason: "number out of range",
                });
            }
        }
        Value::Array(items) => {
            let mut arr = toml_edit::Array::new();
            for item in items {
                arr.push(to_toml(item, key)?);
            }
            arr.into()
        }
        Value::Null | Value::Object(_) => {
            return Err(ConfigError::BadKey {
                key: key.to_owned(),
                reason: "tables inside arrays are not supported",
            });
        }
    })
}

fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let tmp: PathBuf = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}
