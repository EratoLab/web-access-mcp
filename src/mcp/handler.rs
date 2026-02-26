//! ServerHandler implementation for BrowserSession

use crate::browser::BrowserSession;
use log::debug;
use rmcp::{
    ServerHandler,
    handler::server::tool::ToolRouter,
    model::{ServerCapabilities, ServerInfo},
    tool_handler,
};
use std::sync::{Arc, Mutex};

/// MCP Server wrapper for BrowserSession
///
/// This struct holds a browser session and provides thread-safe access
/// for MCP tool execution.
#[derive(Clone)]
pub struct BrowserServer {
    session: Arc<Mutex<Option<BrowserSession>>>,
    launch_options: crate::browser::LaunchOptions,
    tool_router: ToolRouter<Self>,
}

impl BrowserServer {
    /// Create a new browser server with default launch options
    pub fn new() -> Result<Self, String> {
        Self::with_options(crate::browser::LaunchOptions::default())
    }

    /// Create a new browser server with custom launch options
    pub fn with_options(options: crate::browser::LaunchOptions) -> Result<Self, String> {
        Ok(Self {
            session: Arc::new(Mutex::new(None)),
            launch_options: options,
            tool_router: Self::tool_router(),
        })
    }

    /// Execute a closure with a lazily initialized browser session.
    pub(crate) fn with_session<R, F>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&BrowserSession) -> Result<R, String>,
    {
        let mut session_guard = self
            .session
            .lock()
            .map_err(|_| "Failed to lock browser session".to_string())?;

        if session_guard.is_none() {
            let session = BrowserSession::launch(self.launch_options.clone())
                .map_err(|e| format!("Failed to launch browser: {}", e))?;
            *session_guard = Some(session);
        }

        let session = session_guard
            .as_ref()
            .ok_or_else(|| "Browser session is not initialized".to_string())?;

        f(session)
    }
}

impl Default for BrowserServer {
    fn default() -> Self {
        Self::new().expect("Failed to create default browser server")
    }
}

impl Drop for BrowserServer {
    fn drop(&mut self) {
        debug!("BrowserServer dropped");
    }
}

#[tool_handler]
impl ServerHandler for BrowserServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some("Browser-use MCP Server".into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
