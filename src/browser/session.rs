use crate::browser::config::{BrowserEngine, ConnectionOptions, LaunchOptions};
use crate::dom::DomTree;
use crate::error::{BrowserError, Result};
use crate::tools::{ToolContext, ToolRegistry};
use headless_chrome::{Browser, Tab, protocol::cdp::types::Method};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// Wrapper for Tab and Element to maintain proper lifetime relationships
pub struct TabElement<'a> {
    pub tab: Arc<Tab>,
    pub element: headless_chrome::Element<'a>,
}

/// Browser session that manages a Chrome/Chromium instance
pub struct BrowserSession {
    /// The underlying headless_chrome Browser instance
    browser: Browser,

    /// Browser implementation backing this session.
    browser_engine: BrowserEngine,

    /// Tool registry for executing browser automation tools
    tool_registry: ToolRegistry,

    /// Temporary Chrome profile directory owned by this session.
    _profile_dir: Option<tempfile::TempDir>,

    /// LightPanda process owned by this session when using the LightPanda engine.
    _lightpanda_process: Option<ManagedProcess>,
}

#[derive(Debug, Serialize)]
struct LightpandaGetMarkdown {}

#[derive(Debug, Deserialize)]
struct LightpandaGetMarkdownResult {
    markdown: String,
}

impl Method for LightpandaGetMarkdown {
    const NAME: &'static str = "LP.getMarkdown";

    type ReturnObject = LightpandaGetMarkdownResult;
}

struct ManagedProcess(Child);

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            log::debug!("Failed to kill LightPanda process: {}", e);
        }
        if let Err(e) = self.0.wait() {
            log::debug!("Failed to wait for LightPanda process: {}", e);
        }
    }
}

enum ResolvedBrowser {
    Chrome(PathBuf),
    Lightpanda(PathBuf),
}

static NEXT_BROWSER_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn next_browser_session_id() -> u64 {
    NEXT_BROWSER_SESSION_ID.fetch_add(1, Ordering::Relaxed)
}

impl BrowserSession {
    /// Validate that the configured local browser binary exists without launching it.
    pub fn validate_browser_binary(options: &LaunchOptions) -> Result<()> {
        resolve_browser(options).map(|_| ())
    }

    /// Launch a new browser instance with the given options
    pub fn launch(options: LaunchOptions) -> Result<Self> {
        let resolved_browser = resolve_browser(&options)?;
        match resolved_browser {
            ResolvedBrowser::Chrome(path) => Self::launch_chrome(options, path),
            ResolvedBrowser::Lightpanda(path) => Self::launch_lightpanda(options, path),
        }
    }

    fn launch_chrome(options: LaunchOptions, chrome_path: PathBuf) -> Result<Self> {
        let (chrome_user_data_dir, profile_dir) = if let Some(dir) = options.user_data_dir {
            (Some(dir), None)
        } else {
            let dir = tempfile::Builder::new()
                .prefix("web-access-mcp-chrome-")
                .tempdir()?;
            (Some(dir.path().to_path_buf()), Some(dir))
        };

        let mut launch_opts = headless_chrome::LaunchOptions::default();

        // Ignore default arguments to prevent detection by anti-bot services
        launch_opts
            .ignore_default_args
            .push(OsStr::new("--enable-automation"));
        launch_opts
            .args
            .push(OsStr::new("--disable-blink-features=AutomationControlled"));

        // Set the browser's idle timeout to 1 hour (default is 30 seconds) to prevent the session from closing too soon
        launch_opts.idle_browser_timeout = Duration::from_secs(60 * 60);

        // Configure headless mode
        launch_opts.headless = options.headless;

        // Set window size
        launch_opts.window_size = Some((options.window_width, options.window_height));

        launch_opts.path = Some(chrome_path);

        launch_opts.user_data_dir = chrome_user_data_dir;

        // Set sandbox mode
        launch_opts.sandbox = options.sandbox;

        // Launch browser
        let browser =
            Browser::new(launch_opts).map_err(|e| BrowserError::LaunchFailed(e.to_string()))?;

        browser
            .new_tab()
            .map_err(|e| BrowserError::LaunchFailed(format!("Failed to create tab: {}", e)))?;

        Ok(Self {
            browser,
            browser_engine: BrowserEngine::Chrome,
            tool_registry: ToolRegistry::with_defaults(),
            _profile_dir: profile_dir,
            _lightpanda_process: None,
        })
    }

    fn launch_lightpanda(options: LaunchOptions, lightpanda_path: PathBuf) -> Result<Self> {
        if !options.headless {
            return Err(BrowserError::LaunchFailed(
                "LightPanda is headless-only; remove --headed or use --browser chrome".to_string(),
            ));
        }

        if let Some(user_data_dir) = &options.user_data_dir {
            log::warn!(
                "Ignoring user data directory for LightPanda: {}",
                user_data_dir.display()
            );
        }

        let session_id = next_browser_session_id();
        let port = available_local_port()?;
        let port_arg = port.to_string();
        let mut child = Command::new(&lightpanda_path)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port_arg,
                "--log-level",
                "info",
                "--log-format",
                "logfmt",
            ])
            .env("LIGHTPANDA_DISABLE_TELEMETRY", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                BrowserError::LaunchFailed(format!(
                    "Failed to start LightPanda at {}: {}",
                    lightpanda_path.display(),
                    e
                ))
            })?;

        proxy_lightpanda_logs(&mut child, session_id);

        let version_url = format!("http://127.0.0.1:{port}/json/version");
        let ws_url = wait_for_debug_ws_url(&version_url, options.launch_timeout, &mut child)?;
        let browser = Browser::connect_with_timeout(
            ws_url,
            Duration::from_millis(options.launch_timeout.max(1)),
        )
        .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;

        browser
            .new_tab()
            .map_err(|e| BrowserError::LaunchFailed(format!("Failed to create tab: {}", e)))?;

        Ok(Self {
            browser,
            browser_engine: BrowserEngine::Lightpanda,
            tool_registry: ToolRegistry::with_defaults(),
            _profile_dir: None,
            _lightpanda_process: Some(ManagedProcess(child)),
        })
    }

    /// Connect to an existing browser instance via WebSocket
    pub fn connect(options: ConnectionOptions) -> Result<Self> {
        let browser = Browser::connect(options.ws_url)
            .map_err(|e| BrowserError::ConnectionFailed(e.to_string()))?;

        Ok(Self {
            browser,
            browser_engine: BrowserEngine::Chrome,
            tool_registry: ToolRegistry::with_defaults(),
            _profile_dir: None,
            _lightpanda_process: None,
        })
    }

    /// Launch a browser with default options
    pub fn new() -> Result<Self> {
        Self::launch(LaunchOptions::default())
    }

    /// Get the active tab
    pub fn tab(&self) -> Result<Arc<Tab>> {
        self.get_active_tab()
    }

    /// Create a new tab and set it as active
    pub fn new_tab(&mut self) -> Result<Arc<Tab>> {
        let tab = self.browser.new_tab().map_err(|e| {
            BrowserError::TabOperationFailed(format!("Failed to create tab: {}", e))
        })?;
        Ok(tab)
    }

    /// Get all tabs
    pub fn get_tabs(&self) -> Result<Vec<Arc<Tab>>> {
        let tabs = self
            .browser
            .get_tabs()
            .lock()
            .map_err(|e| BrowserError::TabOperationFailed(format!("Failed to get tabs: {}", e)))?
            .clone();

        Ok(tabs)
    }

    /// Get the currently active tab by checking the document visibility and focus state
    pub fn get_active_tab(&self) -> Result<Arc<Tab>> {
        let tabs = self.get_tabs()?;

        // First pass: check for both visibility and focus (strongest signal)
        for tab in &tabs {
            let result = tab.evaluate(
                "document.visibilityState === 'visible' && document.hasFocus()",
                false,
            );
            match result {
                Ok(remote_object) => {
                    if let Some(value) = remote_object.value {
                        if value.as_bool().unwrap_or(false) {
                            return Ok(tab.clone());
                        }
                    }
                }
                Err(e) => {
                    log::debug!("Failed to check tab status: {}", e);
                    continue;
                }
            }
        }

        // Second pass: check just for visibility (weaker signal, but better than nothing)
        for tab in &tabs {
            let result = tab.evaluate("document.visibilityState === 'visible'", false);
            match result {
                Ok(remote_object) => {
                    if let Some(value) = remote_object.value {
                        if value.as_bool().unwrap_or(false) {
                            return Ok(tab.clone());
                        }
                    }
                }
                Err(_) => continue,
            }
        }

        Err(BrowserError::TabOperationFailed(
            "No active tab found".to_string(),
        ))
    }

    /// Close the active tab
    pub fn close_active_tab(&mut self) -> Result<()> {
        self.tab()?
            .close(true)
            .map_err(|e| BrowserError::TabOperationFailed(format!("Failed to close tab: {}", e)))?;

        Ok(())
    }

    /// Get the underlying Browser instance
    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    /// Navigate to a URL using the active tab
    pub fn navigate(&self, url: &str) -> Result<()> {
        self.tab()?.navigate_to(url).map_err(|e| {
            BrowserError::NavigationFailed(format!("Failed to navigate to {}: {}", url, e))
        })?;

        Ok(())
    }

    /// Wait for navigation to complete
    pub fn wait_for_navigation(&self) -> Result<()> {
        if self.browser_engine == BrowserEngine::Lightpanda {
            return self.wait_for_lightpanda_navigation();
        }

        self.tab()?
            .wait_until_navigated()
            .map_err(|e| BrowserError::NavigationFailed(format!("Navigation timeout: {}", e)))?;

        Ok(())
    }

    /// Return whether this session is backed by LightPanda.
    pub fn is_lightpanda(&self) -> bool {
        self.browser_engine == BrowserEngine::Lightpanda
    }

    /// Extract Markdown using LightPanda's native LP.getMarkdown CDP command.
    pub fn get_lightpanda_markdown(&self) -> Result<String> {
        if !self.is_lightpanda() {
            return Err(BrowserError::InvalidArgument(
                "LP.getMarkdown is only available for LightPanda sessions".to_string(),
            ));
        }

        self.tab()?
            .call_method(LightpandaGetMarkdown {})
            .map(|result| result.markdown)
            .map_err(|e| BrowserError::ToolExecutionFailed {
                tool: "get_page_as_markdown".to_string(),
                reason: format!("LP.getMarkdown failed: {}", e),
            })
    }

    fn wait_for_lightpanda_navigation(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(30_000);
        while Instant::now() < deadline {
            match self.tab()?.evaluate(
                "document.readyState === 'complete' || document.readyState === 'interactive'",
                false,
            ) {
                Ok(remote_object) => {
                    if remote_object
                        .value
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                    {
                        return Ok(());
                    }
                }
                Err(e) => {
                    log::debug!("Failed to check LightPanda document readiness: {}", e);
                }
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        Err(BrowserError::NavigationFailed(
            "Timed out waiting for LightPanda document readiness".to_string(),
        ))
    }

    /// Extract the DOM tree from the active tab
    pub fn extract_dom(&self) -> Result<DomTree> {
        DomTree::from_tab(&self.tab()?)
    }

    /// Extract the DOM tree with a custom ref prefix (for iframe handling)
    pub fn extract_dom_with_prefix(&self, prefix: &str) -> Result<DomTree> {
        DomTree::from_tab_with_prefix(&self.tab()?, prefix)
    }

    /// Find an element by CSS selector using the provided tab
    pub fn find_element<'a>(
        &self,
        tab: &'a Arc<Tab>,
        css_selector: &str,
    ) -> Result<headless_chrome::Element<'a>> {
        tab.find_element(css_selector).map_err(|e| {
            BrowserError::ElementNotFound(format!("Element '{}' not found: {}", css_selector, e))
        })
    }

    /// Get the tool registry
    pub fn tool_registry(&self) -> &ToolRegistry {
        &self.tool_registry
    }

    /// Get mutable tool registry
    pub fn tool_registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.tool_registry
    }

    /// Execute a tool by name
    pub fn execute_tool(
        &self,
        name: &str,
        params: serde_json::Value,
    ) -> Result<crate::tools::ToolResult> {
        let mut context = ToolContext::new(self);
        self.tool_registry.execute(name, params, &mut context)
    }

    /// Navigate back in browser history
    pub fn go_back(&self) -> Result<()> {
        let go_back_js = r#"
            (function() {
                window.history.back();
                return true;
            })()
        "#;

        self.tab()?
            .evaluate(go_back_js, false)
            .map_err(|e| BrowserError::NavigationFailed(format!("Failed to go back: {}", e)))?;

        // Wait a moment for navigation
        std::thread::sleep(std::time::Duration::from_millis(300));

        Ok(())
    }

    /// Navigate forward in browser history
    pub fn go_forward(&self) -> Result<()> {
        let go_forward_js = r#"
            (function() {
                window.history.forward();
                return true;
            })()
        "#;

        self.tab()?
            .evaluate(go_forward_js, false)
            .map_err(|e| BrowserError::NavigationFailed(format!("Failed to go forward: {}", e)))?;

        // Wait a moment for navigation
        std::thread::sleep(std::time::Duration::from_millis(300));

        Ok(())
    }

    /// Close the browser
    pub fn close(&self) -> Result<()> {
        // Note: The Browser struct doesn't have a public close method in headless_chrome
        // The browser will be closed when the Browser instance is dropped
        // We can close all tabs to effectively shut down
        let tabs = self.get_tabs()?;
        for tab in tabs {
            let _ = tab.close(false); // Ignore errors on individual tab closes
        }
        Ok(())
    }
}

fn resolve_browser(options: &LaunchOptions) -> Result<ResolvedBrowser> {
    match options.browser_engine {
        BrowserEngine::Auto => {
            if let Ok(path) = resolve_chrome_path(options.chrome_path.as_deref()) {
                return Ok(ResolvedBrowser::Chrome(path));
            }

            if let Ok(path) = resolve_lightpanda_path(options.chrome_path.as_deref()) {
                return Ok(ResolvedBrowser::Lightpanda(path));
            }

            Err(BrowserError::LaunchFailed(
                "Neither Chrome/Chromium nor LightPanda was found. Install Chrome/Chromium, install LightPanda, or pass --executable-path.".to_string(),
            ))
        }
        BrowserEngine::Chrome => resolve_chrome_path(options.chrome_path.as_deref())
            .map(ResolvedBrowser::Chrome)
            .map_err(|e| {
                BrowserError::LaunchFailed(format!("Chrome requested but unavailable: {e}"))
            }),
        BrowserEngine::Lightpanda => resolve_lightpanda_path(options.chrome_path.as_deref())
            .map(ResolvedBrowser::Lightpanda)
            .map_err(|e| {
                BrowserError::LaunchFailed(format!("LightPanda requested but unavailable: {e}"))
            }),
    }
}

fn resolve_chrome_path(custom_path: Option<&Path>) -> std::result::Result<PathBuf, String> {
    if let Some(path) = custom_path {
        if executable_exists(path) {
            return Ok(path.to_path_buf());
        }
        return Err(format!("{} does not exist", path.display()));
    }

    headless_chrome::browser::default_executable()
}

fn resolve_lightpanda_path(custom_path: Option<&Path>) -> std::result::Result<PathBuf, String> {
    if let Some(path) = custom_path {
        if executable_exists(path) {
            return Ok(path.to_path_buf());
        }
        return Err(format!("{} does not exist", path.display()));
    }

    for var_name in ["LIGHTPANDA", "LIGHTPANDA_BIN"] {
        if let Ok(path) = std::env::var(var_name) {
            let path = PathBuf::from(path);
            if executable_exists(&path) {
                return Ok(path);
            }
        }
    }

    find_in_path("lightpanda").ok_or_else(|| "lightpanda was not found on PATH".to_string())
}

fn executable_exists(path: &Path) -> bool {
    path.is_file()
}

fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary_name);
        if executable_exists(&candidate) {
            return Some(candidate);
        }

        #[cfg(windows)]
        {
            let candidate = dir.join(format!("{binary_name}.exe"));
            if executable_exists(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn available_local_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn proxy_lightpanda_logs(child: &mut Child, session_id: u64) {
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                log::info!("LightPanda session_id={} stdout: {}", session_id, line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr)
                .lines()
                .map_while(std::result::Result::ok)
            {
                log::warn!("LightPanda session_id={} stderr: {}", session_id, line);
            }
        });
    }
}

fn wait_for_debug_ws_url(version_url: &str, timeout_ms: u64, child: &mut Child) -> Result<String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    let mut last_error = None;

    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(BrowserError::LaunchFailed(format!(
                "LightPanda exited before CDP became available: {status}"
            )));
        }

        match fetch_debug_ws_url(version_url) {
            Ok(ws_url) => return Ok(ws_url),
            Err(e) => last_error = Some(e),
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    Err(BrowserError::Timeout(format!(
        "Timed out waiting for LightPanda CDP endpoint at {version_url}: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

fn fetch_debug_ws_url(version_url: &str) -> std::result::Result<String, String> {
    let (host, port, path) = parse_local_http_url(version_url)?;
    let mut stream = TcpStream::connect((host.as_str(), port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| e.to_string())?;

    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    let mut response_bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => response_bytes.extend_from_slice(&buffer[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && !response_bytes.is_empty() =>
            {
                break;
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    let response = String::from_utf8(response_bytes).map_err(|e| e.to_string())?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .ok_or_else(|| "invalid HTTP response".to_string())?;

    let version: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    version
        .get("webSocketDebuggerUrl")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "missing webSocketDebuggerUrl in /json/version response".to_string())
}

fn parse_local_http_url(url: &str) -> std::result::Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "expected http:// URL".to_string())?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| "expected host:port".to_string())?;
    let port = port.parse::<u16>().map_err(|e| e.to_string())?;
    Ok((host.to_string(), port, format!("/{path}")))
}

impl Default for BrowserSession {
    fn default() -> Self {
        Self::new().expect("Failed to create default browser session")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launch_options_builder() {
        let opts = LaunchOptions::new().headless(true).window_size(800, 600);

        assert!(opts.headless);
        assert_eq!(opts.window_width, 800);
        assert_eq!(opts.window_height, 600);
    }

    #[test]
    fn test_connection_options() {
        let opts = ConnectionOptions::new("ws://localhost:9222").timeout(5000);

        assert_eq!(opts.ws_url, "ws://localhost:9222");
        assert_eq!(opts.timeout, 5000);
    }

    #[test]
    #[ignore]
    fn test_get_active_tab() {
        let session = BrowserSession::launch(LaunchOptions::new().headless(true))
            .expect("Failed to launch browser");

        let tab = session.get_active_tab();
        assert!(tab.is_ok());
    }

    // Integration tests (require Chrome to be installed)
    #[test]
    #[ignore] // Ignore by default, run with: cargo test -- --ignored
    fn test_launch_browser() {
        let result = BrowserSession::launch(LaunchOptions::new().headless(true));
        assert!(result.is_ok());
    }

    #[test]
    #[ignore]
    fn test_launch_lightpanda_browser() {
        let session = BrowserSession::launch(
            LaunchOptions::new()
                .browser_engine(BrowserEngine::Lightpanda)
                .headless(true),
        )
        .expect("Failed to launch LightPanda browser");

        let result = session.navigate("https://news.ycombinator.com/");
        assert!(result.is_ok());
    }

    #[test]
    #[ignore]
    fn test_lightpanda_get_page_as_markdown_tool() {
        let session = BrowserSession::launch(
            LaunchOptions::new()
                .browser_engine(BrowserEngine::Lightpanda)
                .headless(true),
        )
        .expect("Failed to launch LightPanda browser");

        let result = session.execute_tool(
            "get_page_as_markdown",
            serde_json::json!({
                "url": "https://news.ycombinator.com/",
                "wait_for_load": true,
                "page": 1,
                "page_size": 100000
            }),
        );
        let result = result.expect("Tool execution failed");
        assert!(result.success, "{result:?}");
        let data = result.data.expect("Missing tool result data");
        assert_eq!(data["title"], "Hacker News");
        assert_eq!(data["hasMorePages"], false);
        assert!(
            data["markdown"]
                .as_str()
                .unwrap_or_default()
                .contains("comments"),
            "{data:?}"
        );
    }

    #[test]
    #[ignore]
    fn test_navigate() {
        let session = BrowserSession::launch(LaunchOptions::new().headless(true))
            .expect("Failed to launch browser");

        let result = session.navigate("about:blank");
        assert!(result.is_ok());
    }

    #[test]
    #[ignore]
    fn test_new_tab() {
        let mut session = BrowserSession::launch(LaunchOptions::new().headless(true))
            .expect("Failed to launch browser");

        let result = session.new_tab();
        assert!(result.is_ok());

        let tabs = session.get_tabs().expect("Failed to get tabs");
        assert!(tabs.len() >= 2);
    }
}
