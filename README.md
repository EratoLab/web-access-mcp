# web-access-mcp

`web-access-mcp` is a Rust-based Model Context Protocol server that gives MCP clients browser-backed access to web pages.

The project currently focuses on one compact tool, `browser_get_page_as_markdown`, which navigates to a URL and returns the page content as Markdown. Internally it launches or connects to a Chrome/Chromium instance through the Chrome DevTools Protocol.

This repository still uses the Rust crate/package name `browser-use`, but the intended product surface is the MCP server binary.

## Current status

- MCP server implemented with `rmcp` 1.5.
- Supported transports: `stdio` and Streamable HTTP.
- SSE transport is no longer supported. `rmcp` 1.5 removed the old SSE server transport.
- Streamable HTTP enables Host header verification by default. Local loopback hosts are allowed by default; use `--allowed-host` for public or proxied deployments, or `--skip-host-header-verification` only when you intentionally want to disable that protection.
- Browser automation is synchronous and backed by `headless_chrome`.
- The exposed MCP tool surface is intentionally small.

## Requirements

- Rust toolchain
- Chrome or Chromium installed, unless connecting to an existing browser through CDP/WebSocket

## Running the MCP server

```bash
# stdio transport, headless browser
cargo run --bin mcp-server

# stdio transport, visible browser
cargo run --bin mcp-server -- --headed

# Streamable HTTP transport
cargo run --bin mcp-server -- --transport http --host 127.0.0.1 --port 3000

# Streamable HTTP behind a hostname or proxy
cargo run --bin mcp-server -- --transport http --allowed-host example.com --allowed-host example.com:3000
```

## MCP tool

### `browser_get_page_as_markdown`

Navigates to a URL and returns the resulting page content as Markdown.

This combines navigation and content extraction in one tool so MCP clients can retrieve web page content with a single call.

## Command line flags

| Flag | Default | Description |
| --- | --- | --- |
| `--headed`, `-H` | `false` | Launch Chrome/Chromium in headed mode instead of headless mode. |
| `--executable-path <PATH>` | unset | Path to a custom Chrome/Chromium executable. |
| `--cdp-endpoint <URL>` | unset | CDP endpoint URL for connecting to a remote browser. |
| `--ws-endpoint <URL>` | unset | WebSocket endpoint URL for connecting to a remote browser. |
| `--user-data-dir <DIR>` | unset | Persistent browser profile directory. |
| `--transport <stdio\|http>`, `-t <stdio\|http>` | `stdio` | MCP transport to use. |
| `--port <PORT>`, `-p <PORT>` | `3000` | Port for Streamable HTTP transport. |
| `--host <HOST>` | `127.0.0.1` | Address to bind for Streamable HTTP transport. |
| `--http-path <PATH>` | `/mcp` | Streamable HTTP endpoint path. |
| `--allowed-host <HOST>` | loopback hosts | Allowed HTTP `Host` header value. Can be repeated. Supports hostnames and `host:port` authorities. |
| `--skip-host-header-verification` | `false` | Disable Streamable HTTP Host header verification. Not recommended for public deployments. |
| `--session-inactivity-timeout-secs <SECONDS>` | `1800` | Streamable HTTP session inactivity timeout. Set to `0` to disable automatic cleanup. |
| `--log-file <PATH>` | `browser-use-mcp.log` | Log file path for stdio mode. |

## Library usage

The crate also exposes lower-level browser automation APIs:

```rust
use browser_use::browser::BrowserSession;

let session = BrowserSession::launch(Default::default())?;
session.navigate("https://example.com", None)?;

let dom = session.extract_dom()?;
```

The library API exists, but the main maintained use case in this repository is the MCP server.

## Acknowledgments

This project was inspired by and references [agent-infra/mcp-server-browser](https://github.com/bytedance/UI-TARS-desktop/tree/main/packages/agent-infra/mcp-servers/browser).

## License

MIT
