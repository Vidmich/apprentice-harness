//! The registry: every tool the daemon knows, by name, with its compiled
//! validator. Names sort deterministically (a `BTreeMap`), which keeps
//! the mentor's `tools` array — the first part of the cached prefix —
//! byte-identical between calls.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use super::schema::{ToolValidator, is_valid_name};
use super::{Tool, ToolSpec};
use crate::mentor::{CacheFlag, ToolDef};

/// A tool as `tools.list` reports it.
pub use apprentice_api::types::ToolInfo;

/// One registered tool.
#[derive(Debug, Clone)]
pub struct Entry {
    pub spec: ToolSpec,
    pub tool: Arc<dyn Tool>,
    pub validator: Arc<ToolValidator>,
}

impl std::fmt::Debug for dyn Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tool({})", self.spec().name)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("invalid tool name {0:?}: snake_case ASCII, at most 64 characters")]
    InvalidName(String),
    #[error("tool {0:?} is already registered")]
    Duplicate(String),
    #[error("tool {name:?}: invalid input_schema: {reason}")]
    InvalidSchema { name: String, reason: String },
}

/// Tools by name. Shared behind an `Arc` and populated at start-up;
/// registration after that is allowed but rare (tests, plug-ins).
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: RwLock<BTreeMap<String, Entry>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Entry>> {
        self.tools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Adds `tool`, compiling its schema.
    ///
    /// # Errors
    /// Bad name, duplicate name or an invalid schema; the registry is
    /// unchanged.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<(), RegistryError> {
        let spec = tool.spec();
        if !is_valid_name(&spec.name) {
            return Err(RegistryError::InvalidName(spec.name));
        }
        let validator = ToolValidator::compile(&spec.input_schema).map_err(|reason| {
            RegistryError::InvalidSchema {
                name: spec.name.clone(),
                reason,
            }
        })?;
        let mut tools = self
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tools.contains_key(&spec.name) {
            return Err(RegistryError::Duplicate(spec.name));
        }
        tools.insert(
            spec.name.clone(),
            Entry {
                spec,
                tool,
                validator: Arc::new(validator),
            },
        );
        Ok(())
    }

    /// Registers each tool in turn; stops at the first error.
    ///
    /// # Errors
    /// See [`Self::register`].
    pub fn register_all<I>(&self, tools: I) -> Result<(), RegistryError>
    where
        I: IntoIterator<Item = Arc<dyn Tool>>,
    {
        tools.into_iter().try_for_each(|t| self.register(t))
    }

    pub fn get(&self, name: &str) -> Option<Entry> {
        self.read().get(name).cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.read().contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// Names in registry order.
    pub fn names(&self) -> Vec<String> {
        self.read().keys().cloned().collect()
    }

    /// Every spec, sorted by name.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.read().values().map(|e| e.spec.clone()).collect()
    }

    /// Every tool with whether `disabled` (the `tools.disabled` config)
    /// leaves it enabled; sorted by name.
    pub fn list(&self, disabled: &[String]) -> Vec<ToolInfo> {
        self.read()
            .values()
            .map(|e| {
                let s = &e.spec;
                ToolInfo {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    input_schema: s.input_schema.clone(),
                    risk: s.risk,
                    tags: s.tags.clone(),
                    timeout_s: s.timeout_s,
                    enabled: !disabled.iter().any(|d| d == &s.name),
                }
            })
            .collect()
    }

    /// The mentor's `tools` array: enabled tools sorted by name, none of
    /// them a cache breakpoint (the runtime marks the last one).
    pub fn defs(&self, disabled: &[String]) -> Vec<ToolDef> {
        self.list(disabled)
            .into_iter()
            .filter(|t| t.enabled)
            .map(|t| ToolDef {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema,
                cache: CacheFlag(false),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::tools::{Risk, ToolContext, ToolError, ToolOutput};

    struct Named(&'static str, Value);

    #[async_trait]
    impl Tool for Named {
        fn spec(&self) -> ToolSpec {
            ToolSpec::new(
                self.0,
                format!("{} tool", self.0),
                self.1.clone(),
                Risk::ReadOnly,
            )
        }

        async fn call(
            &self,
            _ctx: &ToolContext,
            _input: Value,
            _cancel: CancellationToken,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    fn obj() -> Value {
        json!({"type": "object"})
    }

    #[test]
    fn registers_in_name_order_and_filters_disabled() {
        let reg = ToolRegistry::new();
        reg.register(Arc::new(Named("zeta", obj()))).unwrap();
        reg.register(Arc::new(Named("alpha", obj()))).unwrap();
        reg.register(Arc::new(Named("mid", obj()))).unwrap();
        assert_eq!(reg.names(), ["alpha", "mid", "zeta"]);
        let listed = reg.list(&["mid".into(), "unknown".into()]);
        assert_eq!(
            listed.iter().map(|t| t.enabled).collect::<Vec<_>>(),
            [true, false, true]
        );
        let defs = reg.defs(&["mid".into()]);
        assert_eq!(
            defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert!(reg.get("mid").is_some());
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn rejects_bad_names_duplicates_and_schemas() {
        let reg = ToolRegistry::new();
        assert!(matches!(
            reg.register(Arc::new(Named("BadName", obj()))),
            Err(RegistryError::InvalidName(_))
        ));
        reg.register(Arc::new(Named("dup", obj()))).unwrap();
        assert!(matches!(
            reg.register(Arc::new(Named("dup", obj()))),
            Err(RegistryError::Duplicate(_))
        ));
        assert!(matches!(
            reg.register(Arc::new(Named("arr", json!({"type": "array"})))),
            Err(RegistryError::InvalidSchema { .. })
        ));
        assert_eq!(reg.len(), 1);
    }
}
