//! `permission.respond`, `tools.rules`, `tools.allow` and `tools.deny`.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    Empty, PermissionRespond, PermissionRespondParams, ToolsAllow, ToolsDeny, ToolsRuleParams,
    ToolsRuleResult, ToolsRules, ToolsRulesParams, ToolsRulesResult,
};
use apprentice_api::server::{Connection, Router};
use apprentice_api::types::{ConfigLayer, RuleEffect, RuleSource, RuleSpec};

use super::rules::{Layer, append_rule, list_builtins};
use super::{PermissionBroker, user_rules_file, workspace_rules_file};
use crate::config::ConfigLoader;
use crate::workspace::Workspace;

/// Handler for the permission methods.
#[derive(Debug, Clone)]
pub struct PermissionService {
    broker: Arc<PermissionBroker>,
    loader: ConfigLoader,
}

impl PermissionService {
    pub fn new(broker: Arc<PermissionBroker>, loader: ConfigLoader) -> Self {
        Self { broker, loader }
    }

    fn workspace_file(root: &str) -> Result<std::path::PathBuf, RpcError> {
        let ws = Workspace::open(Path::new(root))?;
        Ok(workspace_rules_file(ws.root()))
    }

    /// `tools.rules`: workspace file (when given), user file, built-ins.
    ///
    /// # Errors
    /// The workspace is not a directory.
    pub fn rules(&self, p: &ToolsRulesParams) -> Result<ToolsRulesResult, RpcError> {
        let mut layers = Vec::new();
        if let Some(root) = &p.workspace {
            layers.push(Layer::load(
                RuleSource::Workspace,
                Self::workspace_file(root)?,
            ));
        }
        layers.push(Layer::load(
            RuleSource::User,
            user_rules_file(self.loader.paths()),
        ));
        let mut rules = Vec::new();
        let mut files = Vec::new();
        for layer in &layers {
            rules.extend(layer.list());
            files.push(layer.info());
        }
        rules.extend(list_builtins());
        Ok(ToolsRulesResult { rules, files })
    }

    /// `tools.allow` / `tools.deny`: appends the rule to the layer's
    /// file.
    ///
    /// # Errors
    /// No workspace for the workspace layer, a rule that does not
    /// compile, a file that does not parse, or I/O.
    pub fn add_rule(
        &self,
        p: &ToolsRuleParams,
        effect: RuleEffect,
    ) -> Result<ToolsRuleResult, RpcError> {
        let path = match p.layer {
            ConfigLayer::Workspace => {
                let root = p.workspace.as_deref().ok_or_else(|| {
                    RpcError::invalid_params("the workspace layer needs `workspace`")
                })?;
                Self::workspace_file(root)?
            }
            ConfigLayer::User | _ => user_rules_file(self.loader.paths()),
        };
        if p.tool.is_empty() || p.tool.contains(char::is_whitespace) {
            return Err(RpcError::invalid_params(
                "`tool` must be a tool name or `*`",
            ));
        }
        let rule = RuleSpec {
            tool: p.tool.clone(),
            effect,
            r#match: p.r#match.clone(),
        };
        let line = append_rule(&path, &rule, "added with `harness tools`")
            .map_err(|e| RpcError::config(e.to_string()))?;
        Ok(ToolsRuleResult {
            path: path.to_string_lossy().into_owned(),
            line,
            rule,
        })
    }

    pub fn register(self: Arc<Self>, router: &mut Router) {
        let svc = Arc::clone(&self);
        router.add::<PermissionRespond, _, _>(
            move |_c: Arc<Connection>, p: PermissionRespondParams| {
                let svc = Arc::clone(&svc);
                async move {
                    svc.broker.respond(&p.request_id, p.answer, p.rule)?;
                    Ok(Empty {})
                }
            },
        );
        let svc = Arc::clone(&self);
        router.add::<ToolsRules, _, _>(move |_c: Arc<Connection>, p: ToolsRulesParams| {
            let svc = Arc::clone(&svc);
            async move { svc.rules(&p) }
        });
        let svc = Arc::clone(&self);
        router.add::<ToolsAllow, _, _>(move |_c: Arc<Connection>, p: ToolsRuleParams| {
            let svc = Arc::clone(&svc);
            async move { svc.add_rule(&p, RuleEffect::Allow) }
        });
        let svc = Arc::clone(&self);
        router.add::<ToolsDeny, _, _>(move |_c: Arc<Connection>, p: ToolsRuleParams| {
            let svc = Arc::clone(&svc);
            async move { svc.add_rule(&p, RuleEffect::Deny) }
        });
    }
}
