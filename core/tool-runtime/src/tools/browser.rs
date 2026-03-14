#[cfg(feature = "browser")]
pub mod manager {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;

    use std::time::Duration;

    use futures::StreamExt;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use tokio::sync::Mutex;
    use tracing::info;

    /// Browser configuration.
    #[derive(Debug, Clone)]
    pub struct BrowserConfig {
        pub headless: bool,
        pub timeout_seconds: u64,
        pub max_pages: usize,
        pub allowed_domains: Option<Vec<String>>,
        pub blocked_domains: Vec<String>,
        pub viewport_width: u32,
        pub viewport_height: u32,
        pub screenshot_dir: PathBuf,
        pub user_agent: Option<String>,
    }

    impl Default for BrowserConfig {
        fn default() -> Self {
            Self {
                headless: true,
                timeout_seconds: 30,
                max_pages: 3,
                allowed_domains: None,
                blocked_domains: default_blocklist(),
                viewport_width: 1280,
                viewport_height: 720,
                screenshot_dir: PathBuf::from("screenshots"),
                user_agent: None,
            }
        }
    }

    /// Sanitize a CSS selector for safe interpolation into JavaScript strings.
    /// Escapes characters that could break JS string literals or enable injection.
    pub fn sanitize_css_selector(selector: &str) -> Result<String, BrowserError> {
        if selector.contains('\0') || selector.contains('\n') || selector.contains('\r') {
            return Err(BrowserError::Other(
                "Invalid characters in CSS selector".into(),
            ));
        }
        Ok(selector
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('`', "\\`"))
    }

    pub fn default_blocklist() -> Vec<String> {
        vec![
            "doubleclick.net".into(),
            "googlesyndication.com".into(),
            "facebook.com/tr".into(),
            "analytics.google.com".into(),
            "adservice.google.com".into(),
        ]
    }

    /// Page info returned after navigation.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PageInfo {
        pub url: String,
        pub title: String,
    }

    /// Screenshot result.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ScreenshotResult {
        pub path: PathBuf,
        pub width: u32,
        pub height: u32,
        pub size_bytes: u64,
        pub base64: Option<String>,
    }

    /// A link extracted from a page.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Link {
        pub text: String,
        pub href: String,
    }

    /// Browser automation errors.
    #[derive(Debug, thiserror::Error)]
    pub enum BrowserError {
        #[error("Browser launch failed: {0}")]
        LaunchFailed(String),
        #[error("Navigation failed: {0}")]
        NavigationFailed(String),
        #[error("Element not found: {0}")]
        ElementNotFound(String),
        #[error("Timeout: {0}")]
        Timeout(String),
        #[error("Domain not allowed: {0}")]
        DomainBlocked(String),
        #[error("Max pages exceeded")]
        MaxPagesExceeded,
        #[error("No page open — navigate to a URL first")]
        NoPage,
        #[error("{0}")]
        Other(String),
    }

    /// Manages the browser lifecycle and page interactions.
    pub struct BrowserManager {
        browser: Option<chromiumoxide::Browser>,
        page: Option<chromiumoxide::Page>,
        config: BrowserConfig,
        last_used: Option<Instant>,
        _handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl BrowserManager {
        pub fn new(config: BrowserConfig) -> Self {
            Self {
                browser: None,
                page: None,
                config,
                last_used: None,
                _handle: None,
            }
        }

        /// Check if a domain is allowed by the config.
        pub fn check_domain(&self, url_str: &str) -> Result<(), BrowserError> {
            let parsed = url::Url::parse(url_str)
                .map_err(|e| BrowserError::NavigationFailed(format!("Invalid URL: {}", e)))?;

            let domain = parsed
                .host_str()
                .ok_or_else(|| BrowserError::NavigationFailed("No host in URL".into()))?;

            // Check blocklist
            for blocked in &self.config.blocked_domains {
                if domain.ends_with(blocked) {
                    return Err(BrowserError::DomainBlocked(domain.into()));
                }
            }

            // Check allowlist if set
            if let Some(allowed) = &self.config.allowed_domains {
                if !allowed.iter().any(|a| domain.ends_with(a)) {
                    return Err(BrowserError::DomainBlocked(format!(
                        "{} not in allowed domains",
                        domain
                    )));
                }
            }

            Ok(())
        }

        /// Launch browser if not already running.
        pub async fn ensure_browser(&mut self) -> Result<(), BrowserError> {
            if self.browser.is_some() {
                self.last_used = Some(Instant::now());
                return Ok(());
            }

            info!(
                headless = self.config.headless,
                "Launching browser..."
            );

            let mut builder = chromiumoxide::BrowserConfig::builder();

            if self.config.headless {
                builder = builder.arg("--headless=new");
            }

            builder = builder
                .arg("--no-sandbox")
                .arg("--disable-gpu")
                .arg("--disable-dev-shm-usage")
                .arg(format!(
                    "--window-size={},{}",
                    self.config.viewport_width, self.config.viewport_height
                ))
                .no_sandbox();

            if let Some(ua) = &self.config.user_agent {
                builder = builder.arg(format!("--user-agent={}", ua));
            }

            let browser_config = builder
                .build()
                .map_err(|e| BrowserError::LaunchFailed(format!("{}", e)))?;

            let (browser, mut handler) =
                chromiumoxide::Browser::launch(browser_config)
                    .await
                    .map_err(|e| {
                        BrowserError::LaunchFailed(format!(
                            "{}. Is Chrome/Chromium installed? Set CHROME_PATH or install Google Chrome.",
                            e
                        ))
                    })?;

            // Spawn the handler to process browser events
            let handle = tokio::spawn(async move {
                while let Some(_event) = handler.next().await {
                    // Process browser events
                }
            });

            self.browser = Some(browser);
            self._handle = Some(handle);
            self.last_used = Some(Instant::now());
            info!("Browser launched successfully");

            Ok(())
        }

        /// Navigate to a URL.
        pub async fn navigate(&mut self, url: &str) -> Result<PageInfo, BrowserError> {
            self.check_domain(url)?;
            self.ensure_browser().await?;

            // Close previous page to prevent tab leaks
            if let Some(old_page) = self.page.take() {
                let _ = old_page.close().await;
            }

            let browser = self.browser.as_ref().unwrap();
            let page = browser
                .new_page(url)
                .await
                .map_err(|e| BrowserError::NavigationFailed(e.to_string()))?;

            // Wait for page to load
            page.wait_for_navigation()
                .await
                .map_err(|e| BrowserError::NavigationFailed(e.to_string()))?;

            let title = page
                .get_title()
                .await
                .map_err(|e| BrowserError::Other(e.to_string()))?
                .unwrap_or_default();

            let page_url = page.url().await
                .map_err(|e| BrowserError::Other(e.to_string()))?
                .map(|u| u.to_string())
                .unwrap_or_else(|| url.to_string());

            let info = PageInfo {
                url: page_url,
                title,
            };

            self.page = Some(page);
            self.last_used = Some(Instant::now());

            Ok(info)
        }

        /// Take a screenshot of the current page.
        pub async fn screenshot(&self, full_page: bool) -> Result<ScreenshotResult, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            // Ensure screenshot directory exists
            std::fs::create_dir_all(&self.config.screenshot_dir)
                .map_err(|e| BrowserError::Other(format!("Cannot create screenshot dir: {}", e)))?;

            let filename = format!("shot_{}.png", ulid::Ulid::new());
            let path = self.config.screenshot_dir.join(&filename);

            let params = if full_page {
                chromiumoxide::page::ScreenshotParams::builder()
                    .full_page(true)
                    .build()
            } else {
                chromiumoxide::page::ScreenshotParams::builder().build()
            };

            let png_data = page
                .screenshot(params)
                .await
                .map_err(|e| BrowserError::Other(format!("Screenshot failed: {}", e)))?;

            std::fs::write(&path, &png_data)
                .map_err(|e| BrowserError::Other(format!("Cannot save screenshot: {}", e)))?;

            let b64 = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &png_data,
            );

            Ok(ScreenshotResult {
                path,
                width: self.config.viewport_width,
                height: self.config.viewport_height,
                size_bytes: png_data.len() as u64,
                base64: Some(b64),
            })
        }

        /// Extract text content from the page.
        pub async fn extract_text(
            &self,
            selector: Option<&str>,
        ) -> Result<String, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            let js = if let Some(sel) = selector {
                let safe_sel = sanitize_css_selector(sel)?;
                format!(
                    r#"(() => {{
                        const el = document.querySelector('{}');
                        return el ? el.innerText : 'Element not found: {}';
                    }})()"#,
                    safe_sel, safe_sel
                )
            } else {
                "document.body.innerText".to_string()
            };

            let result = page
                .evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("Text extraction failed: {}", e)))?;

            let text = result
                .into_value::<String>()
                .unwrap_or_default();

            // Truncate to 50KB
            let text = if text.len() > 51200 {
                format!(
                    "{}... [truncated, total {} chars]",
                    &text[..51200],
                    text.len()
                )
            } else {
                text
            };

            Ok(text)
        }

        /// Extract all links from the page.
        pub async fn extract_links(&self) -> Result<Vec<Link>, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            let js = r#"
                Array.from(document.querySelectorAll('a[href]')).slice(0, 100).map(a => ({
                    text: (a.innerText || a.textContent || '').trim().substring(0, 200),
                    href: a.href
                }))
            "#;

            let result = page
                .evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("Link extraction failed: {}", e)))?;

            let links: Vec<Link> = result
                .into_value::<Vec<Link>>()
                .unwrap_or_default();

            Ok(links)
        }

        /// Click an element by CSS selector.
        pub async fn click(&self, selector: &str) -> Result<Option<String>, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            // Check element exists first
            let safe_sel = sanitize_css_selector(selector)?;
            let exists_js = format!(
                "document.querySelector('{}') !== null",
                safe_sel
            );
            let exists = page
                .evaluate(exists_js)
                .await
                .map_err(|e| BrowserError::Other(e.to_string()))?
                .into_value::<bool>()
                .unwrap_or(false);

            if !exists {
                return Err(BrowserError::ElementNotFound(selector.to_string()));
            }

            // Click the element
            let click_js = format!("document.querySelector('{}').click()", safe_sel);
            page.evaluate(click_js)
                .await
                .map_err(|e| BrowserError::Other(format!("Click failed: {}", e)))?;

            // Small delay for potential navigation
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // Get current URL after click (may have navigated)
            let new_url = page.url().await
                .map_err(|e| BrowserError::Other(e.to_string()))?
                .map(|u| u.to_string());

            Ok(new_url)
        }

        /// Type text into an input field.
        pub async fn type_text(
            &self,
            selector: &str,
            text: &str,
        ) -> Result<(), BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            // Focus and set value via JS
            let safe_sel = sanitize_css_selector(selector)?;
            let js = format!(
                r#"(() => {{
                    const el = document.querySelector('{}');
                    if (!el) return false;
                    el.focus();
                    el.value = {};
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return true;
                }})()"#,
                safe_sel,
                serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", text))
            );

            let result = page
                .evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("Type failed: {}", e)))?
                .into_value::<bool>()
                .unwrap_or(false);

            if !result {
                return Err(BrowserError::ElementNotFound(selector.to_string()));
            }

            Ok(())
        }

        /// Get current page info.
        pub async fn page_info(&self) -> Result<PageInfo, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            let title = page
                .get_title()
                .await
                .map_err(|e| BrowserError::Other(e.to_string()))?
                .unwrap_or_default();

            let url = page.url().await
                .map_err(|e| BrowserError::Other(e.to_string()))?
                .map(|u| u.to_string())
                .unwrap_or_default();

            Ok(PageInfo { url, title })
        }

        /// Execute arbitrary JavaScript on the current page.
        pub async fn execute_js(&self, js: &str) -> Result<Value, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            let result = page
                .evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("JS execution failed: {}", e)))?;

            let value = result
                .into_value::<Value>()
                .unwrap_or(Value::Null);

            Ok(value)
        }

        /// Wait for a CSS selector to appear in the DOM.
        pub async fn wait_for_selector(
            &self,
            selector: &str,
            timeout_ms: u64,
        ) -> Result<bool, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;
            let safe_sel = sanitize_css_selector(selector)?;
            let check_js = format!("document.querySelector('{}') !== null", safe_sel);
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);

            loop {
                let found = page
                    .evaluate(check_js.as_str())
                    .await
                    .map_err(|e| BrowserError::Other(e.to_string()))?
                    .into_value::<bool>()
                    .unwrap_or(false);

                if found {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    return Err(BrowserError::Timeout(format!(
                        "Selector '{}' not found within {}ms",
                        selector, timeout_ms
                    )));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }

        /// Scroll the page or scroll an element into view.
        pub async fn scroll(
            &self,
            direction: &str,
            amount: Option<i32>,
            selector: Option<&str>,
        ) -> Result<(), BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            let js = match direction {
                "down" => format!("window.scrollBy(0, {})", amount.unwrap_or(500)),
                "up" => format!("window.scrollBy(0, -{})", amount.unwrap_or(500)),
                "top" => "window.scrollTo(0, 0)".to_string(),
                "bottom" => "window.scrollTo(0, document.body.scrollHeight)".to_string(),
                "element" => {
                    let sel = selector.ok_or_else(|| {
                        BrowserError::Other("selector required for element scroll".into())
                    })?;
                    let safe_sel = sanitize_css_selector(sel)?;
                    format!(
                        r#"(() => {{
                            const el = document.querySelector('{}');
                            if (el) el.scrollIntoView({{behavior:'smooth',block:'center'}});
                            return !!el;
                        }})()"#,
                        safe_sel
                    )
                }
                _ => {
                    return Err(BrowserError::Other(format!(
                        "Unknown scroll direction: {}. Use: up, down, top, bottom, element",
                        direction
                    )));
                }
            };

            page.evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("Scroll failed: {}", e)))?;

            Ok(())
        }

        /// Navigate back in browser history.
        pub async fn go_back(&self) -> Result<PageInfo, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            page.evaluate("window.history.back()")
                .await
                .map_err(|e| BrowserError::Other(format!("Go back failed: {}", e)))?;

            // Wait for navigation
            tokio::time::sleep(Duration::from_millis(500)).await;

            self.page_info().await
        }

        /// Select an option in a <select> dropdown.
        pub async fn select_option(
            &self,
            selector: &str,
            value: &str,
            by: &str,
        ) -> Result<(), BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;
            let safe_sel = sanitize_css_selector(selector)?;
            let safe_val =
                serde_json::to_string(value).unwrap_or_else(|_| format!("\"{}\"", value));

            let js = match by {
                "value" => format!(
                    r#"(() => {{
                        const el = document.querySelector('{sel}');
                        if (!el) return false;
                        el.value = {val};
                        el.dispatchEvent(new Event('change', {{bubbles: true}}));
                        return true;
                    }})()"#,
                    sel = safe_sel,
                    val = safe_val
                ),
                "text" => format!(
                    r#"(() => {{
                        const el = document.querySelector('{sel}');
                        if (!el) return false;
                        const opt = Array.from(el.options).find(o => o.text === {val});
                        if (!opt) return false;
                        el.value = opt.value;
                        el.dispatchEvent(new Event('change', {{bubbles: true}}));
                        return true;
                    }})()"#,
                    sel = safe_sel,
                    val = safe_val
                ),
                _ => {
                    return Err(BrowserError::Other(format!(
                        "Unknown select mode: {}. Use: value, text",
                        by
                    )));
                }
            };

            let result = page
                .evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("Select failed: {}", e)))?
                .into_value::<bool>()
                .unwrap_or(false);

            if !result {
                return Err(BrowserError::ElementNotFound(format!(
                    "{} (option {}={})",
                    selector, by, value
                )));
            }

            Ok(())
        }

        /// Extract HTML content from the page or a specific element.
        pub async fn extract_html(
            &self,
            selector: Option<&str>,
        ) -> Result<String, BrowserError> {
            let page = self.page.as_ref().ok_or(BrowserError::NoPage)?;

            let js = if let Some(sel) = selector {
                let safe_sel = sanitize_css_selector(sel)?;
                format!(
                    r#"(() => {{
                        const el = document.querySelector('{}');
                        return el ? el.outerHTML : 'Element not found: {}';
                    }})()"#,
                    safe_sel, safe_sel
                )
            } else {
                "document.documentElement.outerHTML".to_string()
            };

            let result = page
                .evaluate(js)
                .await
                .map_err(|e| BrowserError::Other(format!("HTML extraction failed: {}", e)))?;

            let html = result.into_value::<String>().unwrap_or_default();

            // Truncate to 100KB (HTML is more verbose than text)
            let html = if html.len() > 102400 {
                format!(
                    "{}... [truncated, total {} chars]",
                    &html[..102400],
                    html.len()
                )
            } else {
                html
            };

            Ok(html)
        }

        /// Close the browser.
        pub async fn close(&mut self) -> Result<(), BrowserError> {
            self.page = None;
            if let Some(mut browser) = self.browser.take() {
                let _ = browser.close().await;
            }
            if let Some(handle) = self._handle.take() {
                handle.abort();
            }
            info!("Browser closed");
            Ok(())
        }

        /// Close browser after idle timeout.
        pub async fn maybe_close_idle(&mut self, idle_timeout: std::time::Duration) {
            if let Some(last_used) = self.last_used {
                if last_used.elapsed() > idle_timeout {
                    info!("Closing idle browser");
                    let _ = self.close().await;
                }
            }
        }
    }

    /// Shared browser manager type.
    pub type SharedBrowserManager = Arc<Mutex<BrowserManager>>;
}

// Re-export for convenience
#[cfg(feature = "browser")]
pub use manager::*;

// ──────────────────────────────────────────────────────────────────
// Browser Tools (6 tools implementing the Tool trait)
// ──────────────────────────────────────────────────────────────────

#[cfg(feature = "browser")]
pub mod tools {
    use super::manager::*;
    use crate::{Tool, ToolContext, ToolDefinition, ToolError, ToolOutput};
    use serde_json::{json, Value};

    // ── browser_navigate ──────────────────────────────────────────
    pub struct BrowserNavigateTool {
        browser: SharedBrowserManager,
    }

    impl BrowserNavigateTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserNavigateTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_navigate".into(),
                name: "browser_navigate".into(),
                description: "Navigate to a URL in the browser. Opens the page and returns its title and URL.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "URL to navigate to (e.g., https://example.com)"
                        }
                    },
                    "required": ["url"]
                }),
                required_permissions: vec!["browser:navigate".into()],
                trust_level: 1,
                idempotent: false,
                timeout_seconds: 30,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("url").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'url' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let url = args["url"].as_str().unwrap();

            let mut browser = self.browser.lock().await;
            let info = browser
                .navigate(url)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({
                    "url": info.url,
                    "title": info.title,
                }),
                tokens_used: None,
            })
        }
    }

    // ── browser_screenshot ────────────────────────────────────────
    pub struct BrowserScreenshotTool {
        browser: SharedBrowserManager,
    }

    impl BrowserScreenshotTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserScreenshotTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_screenshot".into(),
                name: "browser_screenshot".into(),
                description: "Take a screenshot of the current page in the browser.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "full_page": {
                            "type": "boolean",
                            "description": "Capture full page (true) or viewport only (false, default)"
                        }
                    }
                }),
                required_permissions: vec!["browser:screenshot".into()],
                trust_level: 1,
                idempotent: true,
                timeout_seconds: 15,
            }
        }

        fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let full_page = args
                .get("full_page")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let browser = self.browser.lock().await;
            let screenshot = browser
                .screenshot(full_page)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            let mut result = json!({
                "path": screenshot.path.to_string_lossy(),
                "width": screenshot.width,
                "height": screenshot.height,
                "size_bytes": screenshot.size_bytes,
            });

            if let Some(b64) = &screenshot.base64 {
                result["base64_png"] = Value::String(b64.clone());
                result["hint"] =
                    Value::String("Screenshot saved. The base64_png field contains the image data.".into());
            }

            Ok(ToolOutput::Success {
                result,
                tokens_used: None,
            })
        }
    }

    // ── browser_extract_text ──────────────────────────────────────
    pub struct BrowserExtractTextTool {
        browser: SharedBrowserManager,
    }

    impl BrowserExtractTextTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserExtractTextTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_extract_text".into(),
                name: "browser_extract_text".into(),
                description: "Extract text content from the current page or a specific element.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "selector": {
                            "type": "string",
                            "description": "CSS selector to extract text from (optional, defaults to full page body)"
                        }
                    }
                }),
                required_permissions: vec!["browser:read".into()],
                trust_level: 0,
                idempotent: true,
                timeout_seconds: 15,
            }
        }

        fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let selector = args.get("selector").and_then(|v| v.as_str());

            let browser = self.browser.lock().await;
            let text = browser
                .extract_text(selector)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({
                    "text": text,
                    "length": text.len(),
                }),
                tokens_used: None,
            })
        }
    }

    // ── browser_extract_links ─────────────────────────────────────
    pub struct BrowserExtractLinksTool {
        browser: SharedBrowserManager,
    }

    impl BrowserExtractLinksTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserExtractLinksTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_extract_links".into(),
                name: "browser_extract_links".into(),
                description: "Extract all links from the current page (max 100).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
                required_permissions: vec!["browser:read".into()],
                trust_level: 0,
                idempotent: true,
                timeout_seconds: 15,
            }
        }

        fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
            Ok(())
        }

        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let browser = self.browser.lock().await;
            let links = browser
                .extract_links()
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({
                    "links": links,
                    "count": links.len(),
                }),
                tokens_used: None,
            })
        }
    }

    // ── browser_click ─────────────────────────────────────────────
    pub struct BrowserClickTool {
        browser: SharedBrowserManager,
    }

    impl BrowserClickTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserClickTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_click".into(),
                name: "browser_click".into(),
                description: "Click an element on the page by CSS selector.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "selector": {
                            "type": "string",
                            "description": "CSS selector of the element to click (e.g., 'button.submit', '#login')"
                        }
                    },
                    "required": ["selector"]
                }),
                required_permissions: vec!["browser:interact".into()],
                trust_level: 1,
                idempotent: false,
                timeout_seconds: 10,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("selector").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'selector' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let selector = args["selector"].as_str().unwrap();

            let browser = self.browser.lock().await;
            let new_url = browser
                .click(selector)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            let mut result = json!({ "clicked": true });
            if let Some(url) = new_url {
                result["new_url"] = Value::String(url);
            }

            Ok(ToolOutput::Success {
                result,
                tokens_used: None,
            })
        }
    }

    // ── browser_type ──────────────────────────────────────────────
    pub struct BrowserTypeTool {
        browser: SharedBrowserManager,
    }

    impl BrowserTypeTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserTypeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_type".into(),
                name: "browser_type".into(),
                description: "Type text into an input field on the page.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "selector": {
                            "type": "string",
                            "description": "CSS selector of the input element"
                        },
                        "text": {
                            "type": "string",
                            "description": "Text to type into the input"
                        }
                    },
                    "required": ["selector", "text"]
                }),
                required_permissions: vec!["browser:interact".into()],
                trust_level: 1,
                idempotent: false,
                timeout_seconds: 10,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("selector").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'selector' is required".into()));
            }
            if args.get("text").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'text' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let selector = args["selector"].as_str().unwrap();
            let text = args["text"].as_str().unwrap();

            let browser = self.browser.lock().await;
            browser
                .type_text(selector, text)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({ "typed": true }),
                tokens_used: None,
            })
        }
    }

    // ── browser_execute_js ───────────────────────────────────────
    pub struct BrowserExecuteJsTool {
        browser: SharedBrowserManager,
    }

    impl BrowserExecuteJsTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserExecuteJsTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_execute_js".into(),
                name: "browser_execute_js".into(),
                description: "Execute arbitrary JavaScript on the current page and return the result.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "js": {
                            "type": "string",
                            "description": "JavaScript code to execute on the page"
                        }
                    },
                    "required": ["js"]
                }),
                required_permissions: vec!["browser:execute_js".into()],
                trust_level: 2,
                idempotent: false,
                timeout_seconds: 30,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("js").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'js' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let js = args["js"].as_str().unwrap();

            let browser = self.browser.lock().await;
            let result = browser
                .execute_js(js)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            // Truncate large results
            let result_str = serde_json::to_string(&result).unwrap_or_default();
            let result = if result_str.len() > 51200 {
                json!({
                    "result": format!("{}... [truncated, total {} chars]", &result_str[..51200], result_str.len()),
                    "truncated": true,
                })
            } else {
                json!({
                    "result": result,
                })
            };

            Ok(ToolOutput::Success {
                result,
                tokens_used: None,
            })
        }
    }

    // ── browser_wait_for ─────────────────────────────────────────
    pub struct BrowserWaitForTool {
        browser: SharedBrowserManager,
    }

    impl BrowserWaitForTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserWaitForTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_wait_for".into(),
                name: "browser_wait_for".into(),
                description: "Wait for a CSS selector to appear on the page. Useful for SPAs and dynamic content.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "selector": {
                            "type": "string",
                            "description": "CSS selector to wait for"
                        },
                        "timeout_ms": {
                            "type": "number",
                            "description": "Maximum wait time in milliseconds (default: 10000)"
                        }
                    },
                    "required": ["selector"]
                }),
                required_permissions: vec!["browser:read".into()],
                trust_level: 0,
                idempotent: true,
                timeout_seconds: 60,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("selector").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'selector' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let selector = args["selector"].as_str().unwrap();
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10000);

            let browser = self.browser.lock().await;
            let found = browser
                .wait_for_selector(selector, timeout_ms)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({ "found": found, "selector": selector }),
                tokens_used: None,
            })
        }
    }

    // ── browser_scroll ───────────────────────────────────────────
    pub struct BrowserScrollTool {
        browser: SharedBrowserManager,
    }

    impl BrowserScrollTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserScrollTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_scroll".into(),
                name: "browser_scroll".into(),
                description: "Scroll the page in a direction or scroll a specific element into view.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "direction": {
                            "type": "string",
                            "enum": ["up", "down", "top", "bottom", "element"],
                            "description": "Scroll direction: up, down, top, bottom, or element (to scroll element into view)"
                        },
                        "amount": {
                            "type": "number",
                            "description": "Pixels to scroll (for up/down, default: 500)"
                        },
                        "selector": {
                            "type": "string",
                            "description": "CSS selector of element to scroll into view (required when direction is 'element')"
                        }
                    },
                    "required": ["direction"]
                }),
                required_permissions: vec!["browser:interact".into()],
                trust_level: 1,
                idempotent: false,
                timeout_seconds: 10,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("direction").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'direction' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let direction = args["direction"].as_str().unwrap();
            let amount = args.get("amount").and_then(|v| v.as_i64()).map(|v| v as i32);
            let selector = args.get("selector").and_then(|v| v.as_str());

            let browser = self.browser.lock().await;
            browser
                .scroll(direction, amount, selector)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({ "scrolled": true, "direction": direction }),
                tokens_used: None,
            })
        }
    }

    // ── browser_back ─────────────────────────────────────────────
    pub struct BrowserBackTool {
        browser: SharedBrowserManager,
    }

    impl BrowserBackTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserBackTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_back".into(),
                name: "browser_back".into(),
                description: "Go back to the previous page in browser history.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {}
                }),
                required_permissions: vec!["browser:navigate".into()],
                trust_level: 1,
                idempotent: false,
                timeout_seconds: 15,
            }
        }

        fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
            Ok(())
        }

        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let browser = self.browser.lock().await;
            let info = browser
                .go_back()
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({
                    "url": info.url,
                    "title": info.title,
                }),
                tokens_used: None,
            })
        }
    }

    // ── browser_select_option ────────────────────────────────────
    pub struct BrowserSelectOptionTool {
        browser: SharedBrowserManager,
    }

    impl BrowserSelectOptionTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserSelectOptionTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_select_option".into(),
                name: "browser_select_option".into(),
                description: "Select an option in a <select> dropdown by value or visible text.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "selector": {
                            "type": "string",
                            "description": "CSS selector of the <select> element"
                        },
                        "value": {
                            "type": "string",
                            "description": "The option value or text to select"
                        },
                        "by": {
                            "type": "string",
                            "enum": ["value", "text"],
                            "description": "Select by 'value' attribute or visible 'text' (default: value)"
                        }
                    },
                    "required": ["selector", "value"]
                }),
                required_permissions: vec!["browser:interact".into()],
                trust_level: 1,
                idempotent: false,
                timeout_seconds: 10,
            }
        }

        fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
            if args.get("selector").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'selector' is required".into()));
            }
            if args.get("value").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::ValidationError("'value' is required".into()));
            }
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let selector = args["selector"].as_str().unwrap();
            let value = args["value"].as_str().unwrap();
            let by = args
                .get("by")
                .and_then(|v| v.as_str())
                .unwrap_or("value");

            let browser = self.browser.lock().await;
            browser
                .select_option(selector, value, by)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({ "selected": true, "selector": selector, "value": value }),
                tokens_used: None,
            })
        }
    }

    // ── browser_extract_html ─────────────────────────────────────
    pub struct BrowserExtractHtmlTool {
        browser: SharedBrowserManager,
    }

    impl BrowserExtractHtmlTool {
        pub fn new(browser: SharedBrowserManager) -> Self {
            Self { browser }
        }
    }

    #[async_trait::async_trait]
    impl Tool for BrowserExtractHtmlTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                id: "browser_extract_html".into(),
                name: "browser_extract_html".into(),
                description: "Extract HTML content from the current page or a specific element.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "selector": {
                            "type": "string",
                            "description": "CSS selector to extract HTML from (optional, defaults to full page)"
                        }
                    }
                }),
                required_permissions: vec!["browser:read".into()],
                trust_level: 0,
                idempotent: true,
                timeout_seconds: 15,
            }
        }

        fn validate_args(&self, _args: &Value) -> Result<(), ToolError> {
            Ok(())
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let selector = args.get("selector").and_then(|v| v.as_str());

            let browser = self.browser.lock().await;
            let html = browser
                .extract_html(selector)
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

            Ok(ToolOutput::Success {
                result: json!({
                    "html": html,
                    "length": html.len(),
                }),
                tokens_used: None,
            })
        }
    }
}

// ──────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(feature = "browser")]
mod tests {
    use super::manager::*;

    #[test]
    fn test_check_domain_allowed() {
        let config = BrowserConfig {
            blocked_domains: vec![],
            allowed_domains: None,
            ..Default::default()
        };
        let mgr = BrowserManager::new(config);
        assert!(mgr.check_domain("https://example.com").is_ok());
    }

    #[test]
    fn test_check_domain_blocked() {
        let config = BrowserConfig::default(); // has default blocklist
        let mgr = BrowserManager::new(config);
        assert!(mgr.check_domain("https://doubleclick.net/ads").is_err());
    }

    #[test]
    fn test_check_domain_not_in_allowlist() {
        let config = BrowserConfig {
            allowed_domains: Some(vec!["example.com".into()]),
            blocked_domains: vec![],
            ..Default::default()
        };
        let mgr = BrowserManager::new(config);
        assert!(mgr.check_domain("https://google.com").is_err());
    }

    #[test]
    fn test_check_domain_in_allowlist() {
        let config = BrowserConfig {
            allowed_domains: Some(vec!["example.com".into()]),
            blocked_domains: vec![],
            ..Default::default()
        };
        let mgr = BrowserManager::new(config);
        assert!(mgr.check_domain("https://example.com/page").is_ok());
    }

    #[test]
    fn test_check_domain_empty_allowlist_allows_all() {
        let config = BrowserConfig {
            allowed_domains: None,
            blocked_domains: vec![],
            ..Default::default()
        };
        let mgr = BrowserManager::new(config);
        assert!(mgr.check_domain("https://anything.com").is_ok());
    }

    #[test]
    fn test_check_domain_invalid_url() {
        let config = BrowserConfig::default();
        let mgr = BrowserManager::new(config);
        assert!(mgr.check_domain("not a url").is_err());
    }

    #[test]
    fn test_screenshot_result_serialization() {
        let result = ScreenshotResult {
            path: std::path::PathBuf::from("/tmp/shot.png"),
            width: 1280,
            height: 720,
            size_bytes: 12345,
            base64: Some("abc123".into()),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["width"], 1280);
        assert_eq!(json["height"], 720);
        assert_eq!(json["size_bytes"], 12345);
    }

    #[test]
    fn test_page_info_serialization() {
        let info = PageInfo {
            url: "https://example.com".into(),
            title: "Example Domain".into(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["url"], "https://example.com");
        assert_eq!(json["title"], "Example Domain");
    }

    #[test]
    fn test_link_serialization() {
        let link = Link {
            text: "Click here".into(),
            href: "https://example.com".into(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["text"], "Click here");
        assert_eq!(json["href"], "https://example.com");
    }

    #[test]
    fn test_default_config() {
        let config = BrowserConfig::default();
        assert!(config.headless);
        assert_eq!(config.timeout_seconds, 30);
        assert_eq!(config.max_pages, 3);
        assert!(config.allowed_domains.is_none());
        assert!(!config.blocked_domains.is_empty());
        assert_eq!(config.viewport_width, 1280);
        assert_eq!(config.viewport_height, 720);
        assert!(config.user_agent.is_none());
    }

    #[test]
    fn test_sanitize_css_selector_normal() {
        assert_eq!(sanitize_css_selector("div.class").unwrap(), "div.class");
        assert_eq!(sanitize_css_selector("#id").unwrap(), "#id");
        assert_eq!(
            sanitize_css_selector("input[type='text']").unwrap(),
            "input[type=\\'text\\']"
        );
    }

    #[test]
    fn test_sanitize_css_selector_escapes_dangerous_chars() {
        assert_eq!(
            sanitize_css_selector("a'; alert(1); '").unwrap(),
            "a\\'; alert(1); \\'"
        );
        assert_eq!(sanitize_css_selector("a`b").unwrap(), "a\\`b");
        assert_eq!(sanitize_css_selector("a\\b").unwrap(), "a\\\\b");
    }

    #[test]
    fn test_sanitize_css_selector_rejects_control_chars() {
        assert!(sanitize_css_selector("a\0b").is_err());
        assert!(sanitize_css_selector("a\nb").is_err());
        assert!(sanitize_css_selector("a\rb").is_err());
    }

    #[test]
    fn test_browser_error_display() {
        let err = BrowserError::DomainBlocked("evil.com".into());
        assert_eq!(err.to_string(), "Domain not allowed: evil.com");

        let err = BrowserError::NoPage;
        assert!(err.to_string().contains("No page open"));
    }

    // Tool definition tests (don't need a running browser)
    use super::tools::*;
    use crate::Tool;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn mock_browser_manager() -> SharedBrowserManager {
        Arc::new(Mutex::new(BrowserManager::new(BrowserConfig::default())))
    }

    #[test]
    fn test_navigate_tool_definition() {
        let tool = BrowserNavigateTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_navigate");
        assert_eq!(def.trust_level, 1);
        assert!(def.required_permissions.contains(&"browser:navigate".into()));
    }

    #[test]
    fn test_navigate_tool_validates_url() {
        use crate::Tool;
        let tool = BrowserNavigateTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"url": "https://example.com"}))
            .is_ok());
    }

    #[test]
    fn test_screenshot_tool_definition() {
        let tool = BrowserScreenshotTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_screenshot");
        assert!(def.required_permissions.contains(&"browser:screenshot".into()));
    }

    #[test]
    fn test_extract_text_tool_definition() {
        let tool = BrowserExtractTextTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_extract_text");
        assert_eq!(def.trust_level, 0); // read-only
    }

    #[test]
    fn test_click_tool_validates_selector() {
        use crate::Tool;
        let tool = BrowserClickTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"selector": "button"}))
            .is_ok());
    }

    #[test]
    fn test_type_tool_validates_args() {
        use crate::Tool;
        let tool = BrowserTypeTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"selector": "input", "text": "hello"}))
            .is_ok());
    }

    #[test]
    fn test_execute_js_tool_definition() {
        let tool = BrowserExecuteJsTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_execute_js");
        assert_eq!(def.trust_level, 2); // requires approval
        assert!(def.required_permissions.contains(&"browser:execute_js".into()));
    }

    #[test]
    fn test_execute_js_tool_validates_args() {
        let tool = BrowserExecuteJsTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"js": "return 1"}))
            .is_ok());
    }

    #[test]
    fn test_wait_for_tool_definition() {
        let tool = BrowserWaitForTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_wait_for");
        assert_eq!(def.trust_level, 0);
        assert_eq!(def.timeout_seconds, 60);
    }

    #[test]
    fn test_wait_for_tool_validates_args() {
        let tool = BrowserWaitForTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"selector": ".loaded"}))
            .is_ok());
    }

    #[test]
    fn test_scroll_tool_definition() {
        let tool = BrowserScrollTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_scroll");
        assert_eq!(def.trust_level, 1);
    }

    #[test]
    fn test_scroll_tool_validates_args() {
        let tool = BrowserScrollTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"direction": "down"}))
            .is_ok());
    }

    #[test]
    fn test_back_tool_definition() {
        let tool = BrowserBackTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_back");
        assert_eq!(def.trust_level, 1);
        assert!(def.required_permissions.contains(&"browser:navigate".into()));
    }

    #[test]
    fn test_select_option_tool_definition() {
        let tool = BrowserSelectOptionTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_select_option");
        assert_eq!(def.trust_level, 1);
    }

    #[test]
    fn test_select_option_tool_validates_args() {
        let tool = BrowserSelectOptionTool::new(mock_browser_manager());
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"selector": "select"}))
            .is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"selector": "select", "value": "opt1"}))
            .is_ok());
    }

    #[test]
    fn test_extract_html_tool_definition() {
        let tool = BrowserExtractHtmlTool::new(mock_browser_manager());
        let def = tool.definition();
        assert_eq!(def.id, "browser_extract_html");
        assert_eq!(def.trust_level, 0); // read-only
        assert!(def.required_permissions.contains(&"browser:read".into()));
    }

    #[test]
    fn test_permission_filtering_with_browser_tools() {
        // Verify all 12 browser tools have correct permissions
        let tools: Vec<Box<dyn crate::Tool>> = vec![
            Box::new(BrowserNavigateTool::new(mock_browser_manager())),
            Box::new(BrowserScreenshotTool::new(mock_browser_manager())),
            Box::new(BrowserExtractTextTool::new(mock_browser_manager())),
            Box::new(BrowserExtractLinksTool::new(mock_browser_manager())),
            Box::new(BrowserClickTool::new(mock_browser_manager())),
            Box::new(BrowserTypeTool::new(mock_browser_manager())),
            Box::new(BrowserExecuteJsTool::new(mock_browser_manager())),
            Box::new(BrowserWaitForTool::new(mock_browser_manager())),
            Box::new(BrowserScrollTool::new(mock_browser_manager())),
            Box::new(BrowserBackTool::new(mock_browser_manager())),
            Box::new(BrowserSelectOptionTool::new(mock_browser_manager())),
            Box::new(BrowserExtractHtmlTool::new(mock_browser_manager())),
        ];

        let defs: Vec<_> = tools.iter().map(|t| t.definition()).collect();
        assert_eq!(defs.len(), 12);

        // Check all IDs are unique
        let ids: std::collections::HashSet<_> = defs.iter().map(|d| d.id.clone()).collect();
        assert_eq!(ids.len(), 12);

        // Verify read-only tools have trust_level 0
        let text_tool = defs.iter().find(|d| d.id == "browser_extract_text").unwrap();
        assert_eq!(text_tool.trust_level, 0);
        let links_tool = defs.iter().find(|d| d.id == "browser_extract_links").unwrap();
        assert_eq!(links_tool.trust_level, 0);
        let html_tool = defs.iter().find(|d| d.id == "browser_extract_html").unwrap();
        assert_eq!(html_tool.trust_level, 0);
        let wait_tool = defs.iter().find(|d| d.id == "browser_wait_for").unwrap();
        assert_eq!(wait_tool.trust_level, 0);

        // Verify high-trust tools
        let js_tool = defs.iter().find(|d| d.id == "browser_execute_js").unwrap();
        assert_eq!(js_tool.trust_level, 2);
    }
}
