//! `tools.list` over the registry and the config loader: the tools the
//! daemon has, each with whether the (workspace) config's
//! `tools.disabled` leaves it enabled.

use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{ToolsList, ToolsListParams, ToolsListResult};
use apprentice_api::server::{Connection, Router};

use super::ToolRegistry;
use crate::config::ConfigLoader;

/// Handler for `tools.list`.
#[derive(Debug, Clone)]
pub struct ToolsService {
    registry: Arc<ToolRegistry>,
    loader: ConfigLoader,
}

impl ToolsService {
    pub fn new(registry: Arc<ToolRegistry>, loader: ConfigLoader) -> Self {
        Self { registry, loader }
    }

    /// Lists every tool; `enabled` follows `tools.disabled` resolved for
    /// `p.workspace` (the user config alone when absent).
    ///
    /// # Errors
    /// Config error (-32050) when the config for that workspace does not
    /// load.
    pub fn list(&self, p: &ToolsListParams) -> Result<ToolsListResult, RpcError> {
        let workspace = p.workspace.as_deref().map(std::path::Path::new);
        let config = self.loader.load(workspace)?.config;
        Ok(ToolsListResult {
            tools: self.registry.list(&config.tools.disabled),
        })
    }

    pub fn register(self: Arc<Self>, router: &mut Router) {
        let svc = Arc::clone(&self);
        router.add::<ToolsList, _, _>(move |_c: Arc<Connection>, p: ToolsListParams| {
            let svc = Arc::clone(&svc);
            async move { svc.list(&p) }
        });
    }
}
