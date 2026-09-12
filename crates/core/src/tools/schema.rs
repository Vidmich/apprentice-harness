//! Input validation: each registered tool's `input_schema` is compiled
//! once (draft 2020-12, formats enforced) and every call is checked
//! before the tool sees it, so tools can trust the shape of `input`.

use jsonschema::{Draft, Validator};
use serde_json::Value;

use super::ToolError;

/// Most schema errors reported in one `InvalidInput` message.
const MAX_REPORTED: usize = 5;

/// A compiled `input_schema`.
pub struct ToolValidator {
    inner: Validator,
}

impl std::fmt::Debug for ToolValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolValidator").finish_non_exhaustive()
    }
}

impl ToolValidator {
    /// Compiles `schema`. The schema must be an object schema (the API
    /// requires `type: object` at the top level of a tool's input).
    ///
    /// # Errors
    /// The schema is not valid draft 2020-12 or not an object schema.
    pub fn compile(schema: &Value) -> Result<Self, String> {
        let is_object = schema.get("type").and_then(Value::as_str) == Some("object");
        if !is_object {
            return Err("input_schema must have \"type\": \"object\"".into());
        }
        let inner = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .should_validate_formats(true)
            .build(schema)
            .map_err(|e| e.to_string())?;
        Ok(Self { inner })
    }

    /// Checks `input`; the error names every violation with its path.
    ///
    /// # Errors
    /// [`ToolError::InvalidInput`] listing the violations.
    pub fn validate(&self, input: &Value) -> Result<(), ToolError> {
        let mut errors = self.inner.iter_errors(input).peekable();
        if errors.peek().is_none() {
            return Ok(());
        }
        let mut parts = Vec::new();
        let mut more = 0usize;
        for e in errors {
            if parts.len() == MAX_REPORTED {
                more += 1;
                continue;
            }
            let path = e.instance_path().to_string();
            if path.is_empty() {
                parts.push(e.to_string());
            } else {
                parts.push(format!("{path}: {e}"));
            }
        }
        if more > 0 {
            parts.push(format!("... and {more} more"));
        }
        Err(ToolError::InvalidInput(parts.join("; ")))
    }
}

/// Tool names: `snake_case` ASCII, starting with a letter, at most 64
/// characters (what the API accepts and what reads well in prompts).
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.ends_with('_')
        && !name.contains("__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[test]
    fn accepts_matching_input() {
        let v = ToolValidator::compile(&schema()).unwrap();
        v.validate(&json!({"path": "a.txt", "limit": 3})).unwrap();
        v.validate(&json!({"path": "a.txt"})).unwrap();
    }

    #[test]
    fn reports_every_violation_with_its_path() {
        let v = ToolValidator::compile(&schema()).unwrap();
        let err = v.validate(&json!({"limit": 0, "extra": true})).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ToolError::InvalidInput(_)));
        assert!(msg.contains("\"path\" is a required property"), "{msg}");
        assert!(
            msg.contains("/limit: 0 is less than the minimum of 1"),
            "{msg}"
        );
        assert!(msg.contains("extra"), "{msg}");
    }

    #[test]
    fn caps_the_number_of_reported_errors() {
        let v = ToolValidator::compile(&json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"}, "b": {"type": "string"}, "c": {"type": "string"},
                "d": {"type": "string"}, "e": {"type": "string"}, "f": {"type": "string"},
                "g": {"type": "string"}
            }
        }))
        .unwrap();
        let err = v
            .validate(&json!({"a": 1, "b": 1, "c": 1, "d": 1, "e": 1, "f": 1, "g": 1}))
            .unwrap_err();
        assert!(err.to_string().ends_with("... and 2 more"), "{err}");
    }

    #[test]
    fn rejects_non_object_and_broken_schemas() {
        assert!(ToolValidator::compile(&json!({"type": "string"})).is_err());
        assert!(ToolValidator::compile(&json!({"type": "object", "minProperties": -1})).is_err());
    }

    #[test]
    fn names() {
        for ok in ["read_file", "grep", "git_status2", "a"] {
            assert!(is_valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "ReadFile",
            "read-file",
            "_x",
            "x_",
            "a__b",
            "1st",
            "read file",
        ] {
            assert!(!is_valid_name(bad), "{bad}");
        }
        assert!(!is_valid_name(&"a".repeat(65)));
    }
}
