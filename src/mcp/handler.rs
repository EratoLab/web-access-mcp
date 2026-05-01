//! ServerHandler implementation for BrowserSession

use crate::browser::BrowserSession;
use log::debug;
use rmcp::{
    ServerHandler,
    handler::server::tool::ToolRouter,
    model::{ServerCapabilities, ServerInfo},
    tool_handler,
};
use std::sync::{Mutex, OnceLock};

fn browser_session_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// MCP Server wrapper for BrowserSession
///
/// This struct holds a browser session and provides thread-safe access
/// for MCP tool execution.
#[derive(Clone)]
pub struct BrowserServer {
    launch_options: crate::browser::LaunchOptions,
    #[allow(dead_code)]
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
            launch_options: options,
            tool_router: Self::tool_router(),
        })
    }

    /// Execute a closure with a lazily initialized browser session.
    pub(crate) fn with_session<R, F>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&BrowserSession) -> Result<R, String>,
    {
        let _launch_guard = browser_session_lock()
            .lock()
            .map_err(|_| "Failed to lock browser session launcher".to_string())?;

        let session = BrowserSession::launch(self.launch_options.clone())
            .map_err(|e| format!("Failed to launch browser: {}", e))?;
        let result = f(&session);
        if let Err(e) = session.close() {
            debug!("Failed to close browser session cleanly: {}", e);
        }
        result
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
        let mut info = ServerInfo::default();
        info.instructions = Some("Browser-use MCP Server".into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}
