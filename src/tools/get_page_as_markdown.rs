use crate::error::{BrowserError, Result};
use crate::tools::html_to_markdown::convert_html_to_markdown;
use crate::tools::readability_script::READABILITY_SCRIPT;
use crate::tools::utils::normalize_url;
use crate::tools::{Tool, ToolContext, ToolResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for getting a page as markdown
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetPageAsMarkdownParams {
    /// URL to navigate to
    pub url: String,

    /// Wait for navigation to complete (default: true)
    #[serde(default = "default_wait")]
    pub wait_for_load: bool,

    /// Page number to extract (1-based index, default: 1)
    #[serde(default = "default_page")]
    pub page: usize,

    /// Maximum characters per page (default: 100000)
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

fn default_wait() -> bool {
    true
}

fn default_page() -> usize {
    1
}

fn default_page_size() -> usize {
    100_000
}

impl Default for GetPageAsMarkdownParams {
    fn default() -> Self {
        Self {
            url: String::new(),
            wait_for_load: default_wait(),
            page: default_page(),
            page_size: default_page_size(),
        }
    }
}

/// Tool for navigating to a URL and getting the page as markdown
#[derive(Default)]
pub struct GetPageAsMarkdownTool;

impl Tool for GetPageAsMarkdownTool {
    type Params = GetPageAsMarkdownParams;

    fn name(&self) -> &str {
        "get_page_as_markdown"
    }

    fn execute_typed(
        &self,
        params: GetPageAsMarkdownParams,
        context: &mut ToolContext,
    ) -> Result<ToolResult> {
        // Normalize and navigate to the URL
        let normalized_url = normalize_url(&params.url);

        // Try to navigate - catch navigation errors gracefully
        if let Err(e) = context.session.navigate(&normalized_url) {
            // Check if this is a 404 or DNS error
            let error_msg = e.to_string();
            let is_404_or_dns_error = error_msg.contains("404")
                || error_msg.contains("Not Found")
                || error_msg.contains("net::ERR_NAME_NOT_RESOLVED")
                || error_msg.contains("net::ERR_CONNECTION_REFUSED")
                || error_msg.contains("net::ERR_INTERNET_DISCONNECTED")
                || error_msg.contains("net::ERR_NAME_RESOLUTION_FAILED")
                || error_msg.contains("DNS");

            // For 404 or DNS errors, return a graceful error response instead of bubbling up
            if is_404_or_dns_error {
                return Ok(ToolResult::success_with(serde_json::json!({
                    "error": "PAGE_NOT_FOUND",
                    "message": format!("Unable to access the page: {}", error_msg),
                    "url": normalized_url,
                    "markdown": "",
                    "title": "",
                    "statusCode": if error_msg.contains("404") || error_msg.contains("Not Found") { 404 } else { 0 }
                })));
            }

            // For other errors, bubble them up as before
            return Err(e);
        }

        // Wait for navigation if requested
        if params.wait_for_load {
            if let Err(e) = context.session.wait_for_navigation() {
                let error_msg = e.to_string();
                let is_404_or_dns_error = error_msg.contains("404")
                    || error_msg.contains("Not Found")
                    || error_msg.contains("net::ERR_NAME_NOT_RESOLVED")
                    || error_msg.contains("net::ERR_CONNECTION_REFUSED")
                    || error_msg.contains("net::ERR_INTERNET_DISCONNECTED")
                    || error_msg.contains("net::ERR_NAME_RESOLUTION_FAILED")
                    || error_msg.contains("DNS");

                if is_404_or_dns_error {
                    return Ok(ToolResult::success_with(serde_json::json!({
                        "error": "PAGE_NOT_FOUND",
                        "message": format!("Unable to access the page: {}", error_msg),
                        "url": normalized_url,
                        "markdown": "",
                        "title": "",
                        "statusCode": if error_msg.contains("404") || error_msg.contains("Not Found") { 404 } else { 0 }
                    })));
                }

                return Err(e);
            }
        }

        // Wait for network idle with a timeout
        // Since headless_chrome doesn't have a direct network idle wait,
        // we add a small delay to let dynamic content load
        std::thread::sleep(std::time::Duration::from_millis(1000));

        // Check if we landed on an error page by examining the title and URL
        // Chrome shows error pages with specific patterns
        let tab = context.session.tab()?;
        let page_check = tab.evaluate(
            r#"(function() {
                const title = document.title || '';
                const url = window.location.href || '';
                const bodyText = document.body ? document.body.innerText : '';

                // Check for Chrome error pages
                const isChromeError = title.includes('ERR_') ||
                                     url.includes('chrome-error://') ||
                                     bodyText.includes('ERR_NAME_NOT_RESOLVED') ||
                                     bodyText.includes('ERR_CONNECTION_REFUSED') ||
                                     bodyText.includes('ERR_INTERNET_DISCONNECTED') ||
                                     bodyText.includes('ERR_NAME_RESOLUTION_FAILED');

                // Check for 404 pages (common patterns)
                const is404 = title.toLowerCase().includes('404') ||
                             title.toLowerCase().includes('not found') ||
                             bodyText.toLowerCase().includes('404') ||
                             bodyText.toLowerCase().includes('page not found');

                return {
                    isChromeError: isChromeError,
                    is404: is404,
                    title: title,
                    url: url
                };
            })()"#,
            false,
        );

        if let Ok(check_result) = page_check {
            if let Some(check_value) = check_result.value {
                if let Ok(check_data) =
                    serde_json::from_value::<serde_json::Value>(check_value.clone())
                {
                    let is_chrome_error = check_data
                        .get("isChromeError")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let is_404 = check_data
                        .get("is404")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if is_chrome_error || is_404 {
                        let error_title = check_data
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        return Ok(ToolResult::success_with(serde_json::json!({
                            "error": "PAGE_NOT_FOUND",
                            "message": if is_404 {
                                "The page returned a 404 Not Found error"
                            } else {
                                "Unable to access the page (DNS or connection error)"
                            },
                            "url": normalized_url,
                            "markdown": "",
                            "title": error_title,
                            "statusCode": if is_404 { 404 } else { 0 }
                        })));
                    }
                }
            }
        }

        if context.session.is_lightpanda() {
            let markdown = context.session.get_lightpanda_markdown()?;
            let title = get_page_string(context, "document.title")?;
            let url = get_page_string(context, "window.location.href")?;
            return Ok(markdown_tool_result(
                &markdown, &title, &url, "", "", "", &params,
            ));
        }

        // Inject Readability.js script and the conversion script
        // Use 'var' instead of 'const' to allow redeclaration on subsequent calls
        // This prevents "identifier already declared" errors when calling get_markdown multiple times
        let js_code = format!(
            "var READABILITY_SCRIPT = {};\n{}",
            serde_json::to_string(READABILITY_SCRIPT).unwrap(),
            include_str!("convert_to_markdown.js")
        );

        // Execute the JavaScript to extract and convert content
        let mut extraction_result = match context.session.tab()?.evaluate(&js_code, false) {
            Ok(result) => parse_extraction_result(result.value, result.description, result.Type)?,
            Err(e) => {
                log::debug!(
                    "Readability extraction failed, falling back to full document HTML: {}",
                    e
                );
                extract_full_document(context)?
            }
        };

        // Check if Readability failed
        if extraction_result.readability_failed {
            log::debug!(
                "Readability extraction failed, falling back to full document HTML: {}",
                extraction_result
                    .error
                    .as_deref()
                    .unwrap_or("Readability extraction failed")
            );
            extraction_result = extract_full_document(context)?;
        }

        // Convert the extracted HTML content to Markdown
        let full_markdown = convert_html_to_markdown(&extraction_result.content);

        Ok(markdown_tool_result(
            &full_markdown,
            &extraction_result.title,
            &extraction_result.url,
            &extraction_result.byline,
            &extraction_result.excerpt,
            &extraction_result.site_name,
            &params,
        ))
    }
}

fn markdown_tool_result(
    full_markdown: &str,
    title: &str,
    url: &str,
    byline: &str,
    excerpt: &str,
    site_name: &str,
    params: &GetPageAsMarkdownParams,
) -> ToolResult {
    let total_pages = if full_markdown.is_empty() {
        1
    } else {
        (full_markdown.len() + params.page_size - 1) / params.page_size
    };

    let current_page = params.page.clamp(1, total_pages.max(1));
    let start_idx = (current_page - 1) * params.page_size;
    let end_idx = (start_idx + params.page_size).min(full_markdown.len());

    let mut page_content = if start_idx < full_markdown.len() {
        full_markdown[start_idx..end_idx].to_string()
    } else {
        String::new()
    };

    if current_page == 1 && !title.is_empty() && !page_content.starts_with("# ") {
        page_content = format!("# {}\n\n{}", title, page_content);
    }

    if total_pages > 1 {
        let pagination_info = if current_page < total_pages {
            format!(
                "\n\n---\n\n*Page {} of {}. There are {} more page(s) with additional content.*\n",
                current_page,
                total_pages,
                total_pages - current_page
            )
        } else {
            format!(
                "\n\n---\n\n*Page {} of {}. This is the last page.*\n",
                current_page, total_pages
            )
        };
        page_content.push_str(&pagination_info);
    }

    ToolResult::success_with(serde_json::json!({
        "markdown": page_content,
        "title": title,
        "url": url,
        "currentPage": current_page,
        "totalPages": total_pages,
        "hasMorePages": current_page < total_pages,
        "length": page_content.len(),
        "byline": byline,
        "excerpt": excerpt,
        "siteName": site_name,
    }))
}

fn get_page_string(context: &mut ToolContext, expression: &str) -> Result<String> {
    let result = context
        .session
        .tab()?
        .evaluate(expression, false)
        .map_err(|e| BrowserError::EvaluationFailed(e.to_string()))?;

    Ok(result
        .value
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default())
}

fn parse_extraction_result(
    result_value: Option<serde_json::Value>,
    description: Option<String>,
    remote_type: headless_chrome::protocol::cdp::Runtime::RemoteObjectType,
) -> Result<ExtractionResult> {
    let result_value = result_value.ok_or_else(|| {
        let description = description
            .map(|d| format!("Description: {}", d))
            .unwrap_or_else(|| format!("Type: {:?}", remote_type));

        BrowserError::ToolExecutionFailed {
            tool: "get_page_as_markdown".to_string(),
            reason: format!("No value returned from JavaScript. {}", description),
        }
    })?;

    if let Some(json_str) = result_value.as_str() {
        serde_json::from_str(json_str).map_err(|e| BrowserError::ToolExecutionFailed {
            tool: "get_page_as_markdown".to_string(),
            reason: format!("Failed to parse extraction result: {}", e),
        })
    } else {
        serde_json::from_value(result_value).map_err(|e| BrowserError::ToolExecutionFailed {
            tool: "get_page_as_markdown".to_string(),
            reason: format!("Failed to deserialize extraction result: {}", e),
        })
    }
}

fn extract_full_document(context: &mut ToolContext) -> Result<ExtractionResult> {
    let result = context
        .session
        .tab()?
        .evaluate(
            r#"(function() {
                function textLength(element) {
                    return (element && (element.innerText || element.textContent) || '').trim().length;
                }

                function cloneClean(element) {
                    const clone = element.cloneNode(true);
                    clone.querySelectorAll('script,noscript,style,link,svg,iframe,canvas').forEach(function(el) {
                        el.remove();
                    });
                    clone.querySelectorAll('img').forEach(function(img) {
                        if (!img.getAttribute('alt')) {
                            img.remove();
                        }
                    });
                    return clone;
                }

                const preferredSelectors = [
                    'article',
                    'main',
                    '[role="main"]',
                    '#main',
                    '#content',
                    '#contents',
                    '#article',
                    '#post',
                    '#story',
                    '.content',
                    '.article',
                    '.post',
                    '.entry',
                    '#bigbox'
                ];

                let selected = null;
                for (const selector of preferredSelectors) {
                    const candidates = Array.from(document.querySelectorAll(selector))
                        .filter(function(el) { return textLength(el) > 100; });
                    if (candidates.length > 0) {
                        candidates.sort(function(a, b) { return textLength(b) - textLength(a); });
                        selected = candidates[0];
                        break;
                    }
                }

                if (!selected && document.body) {
                    const candidates = Array.from(document.body.querySelectorAll('section,div,table,td'))
                        .filter(function(el) {
                            const tag = el.tagName.toLowerCase();
                            if (['header', 'footer', 'nav', 'form'].includes(tag)) {
                                return false;
                            }
                            return textLength(el) > 200;
                        });
                    candidates.sort(function(a, b) {
                        const aText = textLength(a);
                        const bText = textLength(b);
                        const aPenalty = a.querySelectorAll('form,input,button').length * 500;
                        const bPenalty = b.querySelectorAll('form,input,button').length * 500;
                        return (bText - bPenalty) - (aText - aPenalty);
                    });
                    selected = candidates[0] || document.body;
                }

                const cleaned = selected ? cloneClean(selected) : null;
                const content = cleaned ? cleaned.outerHTML : '';
                const textContent = selected ? (selected.innerText || selected.textContent || '') : '';
                return JSON.stringify({
                    title: document.title || '',
                    content: content,
                    textContent: textContent,
                    url: window.location.href || '',
                    readabilityFailed: false
                });
            })()"#,
            false,
        )
        .map_err(|e| BrowserError::EvaluationFailed(e.to_string()))?;

    parse_extraction_result(result.value, result.description, result.Type)
}

/// Structure for extraction result returned from JavaScript
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtractionResult {
    title: String,
    content: String,
    text_content: String,
    url: String,
    #[serde(default)]
    excerpt: String,
    #[serde(default)]
    byline: String,
    #[serde(default)]
    site_name: String,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    lang: String,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    published_time: String,
    #[serde(default)]
    readability_failed: bool,
    #[serde(default)]
    error: Option<String>,
}
