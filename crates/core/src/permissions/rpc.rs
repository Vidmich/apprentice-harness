//! `permission.respond`, `tools.rules`, `tools.allow`, `tools.deny` and
//! `tools.remove`.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    Empty, PermissionRespond, PermissionRespondParams, ToolsAllow, ToolsDeny, ToolsRemove,
    ToolsRemoveParams, ToolsRemoveResult, ToolsRuleParams, ToolsRuleResult, ToolsRules,
    ToolsRulesParams, ToolsRulesResult,
};
use apprentice_api::server::{Connection, Router};
use apprentice_api::types::{ConfigLayer, RuleEffect, RuleSource, RuleSpec};

use super::rules::{Layer, append_rule, list_builtins, remove_rule};
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
        let path = self.layer_file(p.layer, p.workspace.as_deref())?;
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

    /// `tools.remove`: deletes one rule of a layer's file.
    ///
    /// # Errors
    /// No workspace for the workspace layer, no such rule, a file that
    /// does not parse, or I/O.
    pub fn remove_rule(&self, p: &ToolsRemoveParams) -> Result<ToolsRemoveResult, RpcError> {
        let path = self.layer_file(p.layer, p.workspace.as_deref())?;
        let rule = remove_rule(&path, p.index).map_err(|e| RpcError::config(e.to_string()))?;
        Ok(ToolsRemoveResult {
            path: path.to_string_lossy().into_owned(),
            rule,
        })
    }

    /// The rules file of a layer.
    fn layer_file(
        &self,
        layer: ConfigLayer,
        workspace: Option<&str>,
    ) -> Result<std::path::PathBuf, RpcError> {
        match layer {
            ConfigLayer::Workspace => {
                let root = workspace.ok_or_else(|| {
                    RpcError::invalid_params("the workspace layer needs `workspace`")
                })?;
                Self::workspace_file(root)
            }
            ConfigLayer::User | _ => Ok(user_rules_file(self.loader.paths())),
        }
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
        let svc = Arc::clone(&self);
        router.add::<ToolsRemove, _, _>(move |_c: Arc<Connection>, p: ToolsRemoveParams| {
            let svc = Arc::clone(&svc);
            async move { svc.remove_rule(&p) }
        });
    }
}
