//! Direct HTTP downloads with Erato's MCP file output annotations.

use super::BrowserServer;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use percent_encoding::percent_decode_str;
use rmcp::{
    ErrorData as McpError,
    handler::server::{common::schema_for_type, wrapper::Parameters},
    model::{CallToolResult, Content},
    tool, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{io::Read, time::Duration};
use url::Url;

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize, JsonSchema)]
struct RetrieveFileParams {
    /// Absolute HTTP or HTTPS URL of the file to download.
    url: String,
}

#[derive(Serialize, JsonSchema)]
struct RetrievedFile {
    #[schemars(extend("contentEncoding" = "base64", "chat.erato/file_content_field" = true))]
    data_base64: String,
    #[schemars(extend("chat.erato/file_name_field" = true))]
    name: String,
    mime_type: String,
}

#[tool_router(router = file_retrieval_router, vis = "pub(super)")]
impl BrowserServer {
    #[tool(
        description = "Download one file (up to 10 MiB) from a direct HTTP(S) URL and return it as an attachment.",
        output_schema = schema_for_type::<RetrievedFile>()
    )]
    async fn retrieve_file(
        &self,
        Parameters(params): Parameters<RetrieveFileParams>,
    ) -> Result<CallToolResult, McpError> {
        let url = Url::parse(params.url.trim())
            .ok()
            .filter(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.username().is_empty()
                    && url.password().is_none()
            })
            .ok_or_else(|| {
                McpError::invalid_params(
                    "Expected an absolute HTTP(S) URL without credentials",
                    None,
                )
            })?;

        let download = tokio::task::spawn_blocking(move || download_file(&url))
            .await
            .map_err(|err| {
                McpError::internal_error(format!("Download task failed: {err}"), None)
            })?;

        match download {
            Ok(file) => {
                let mut result = CallToolResult::success(vec![Content::text("File retrieved.")]);
                result.structured_content = Some(serde_json::json!(file));
                Ok(result)
            }
            Err(err) => Ok(CallToolResult::error(vec![Content::text(err.to_string())])),
        }
    }
}

fn download_file(url: &Url) -> Result<RetrievedFile> {
    let response = ureq::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .request_url("GET", url)
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(status, _) => anyhow!("Download failed with HTTP {status}"),
            ureq::Error::Transport(err) => anyhow!("Download failed: {}", err.kind()),
        })?;

    let status = response.status();
    if !(200..300).contains(&status) || matches!(status, 204..=206) {
        bail!("Expected a complete file response, received HTTP {status}");
    }

    let mime_type = response
        .header("Content-Type")
        .and_then(|value| value.parse::<mime::Mime>().ok())
        .unwrap_or(mime::APPLICATION_OCTET_STREAM)
        .essence_str()
        .to_owned();
    let name = response
        .header("Content-Disposition")
        .and_then(disposition_filename)
        .or_else(|| url_filename(response.get_url()))
        .unwrap_or_else(|| "download".to_owned());

    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("Failed to read the download body")?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!("File exceeds the 10 MiB download limit");
    }

    Ok(RetrievedFile {
        data_base64: STANDARD.encode(bytes),
        name,
        mime_type,
    })
}

fn disposition_filename(header: &str) -> Option<String> {
    // A disposition has the same token and parameter syntax as a media subtype.
    let disposition = format!("application/{header}").parse::<mime::Mime>().ok()?;
    let extended_name = disposition
        .get_param("filename*")
        .and_then(|value| {
            let (charset, rest) = value.as_str().split_once('\'')?;
            let (_, name) = rest.split_once('\'')?;
            charset.eq_ignore_ascii_case("utf-8").then_some(name)
        })
        .and_then(|name| percent_decode_str(name).decode_utf8().ok())
        .and_then(|name| safe_filename(&name));

    extended_name.or_else(|| safe_filename(disposition.get_param("filename")?.as_str()))
}

fn url_filename(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let name = percent_decode_str(url.path_segments()?.next_back()?)
        .decode_utf8()
        .ok()?;
    safe_filename(&name)
}

fn safe_filename(name: &str) -> Option<String> {
    let name = name.trim();
    (!matches!(name, "" | "." | "..")
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control))
    .then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_a_safe_filename_from_the_header_or_url() {
        assert_eq!(
            disposition_filename("attachment; filename=report.pdf; filename*=UTF-8''caf%C3%A9.pdf")
                .as_deref(),
            Some("café.pdf")
        );
        assert_eq!(
            disposition_filename("attachment; filename=\"Quarter; Report.pdf\"").as_deref(),
            Some("Quarter; Report.pdf")
        );
        assert_eq!(
            disposition_filename("attachment; filename=\"../secret\""),
            None
        );
        assert_eq!(
            url_filename("https://example.com/files/report%20one.pdf?token=123").as_deref(),
            Some("report one.pdf")
        );
        assert_eq!(url_filename("https://example.com/%2Fsecret"), None);
    }
}
