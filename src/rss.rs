// RSS处理模块

use crate::models::Article;
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use headless_chrome::{Browser, LaunchOptionsBuilder};
use html2text::from_read;
use reqwest::Url;
use roxmltree::Document;
use rss::{Channel, Item};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::sleep;

// 定义类型别名简化复杂类型
type ArticleCache = Arc<Mutex<HashMap<String, (Instant, Vec<Article>)>>>;

/// RSS错误类型
#[derive(Error, Debug)]
pub enum RssError {
    #[error("HTTP请求错误: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("RSS解析错误: {0}")]
    ParseError(#[from] rss::Error),

    #[error("时间解析错误: {0}")]
    TimeError(#[from] chrono::ParseError),

    #[error("网络错误: {0}")]
    NetworkError(String),

    #[error("XML解析错误: {0}")]
    XmlError(String),

    #[error("Atom解析错误")]
    AtomParseError,

    #[error("未知错误")]
    Unknown,
}

/// RSS获取器
pub struct RssFetcher {
    client: reqwest::Client,

    /// 用户代理
    user_agent: String,

    /// 内存缓存
    cache: ArticleCache,

    cache_ttl: u64,
}

impl RssFetcher {
    pub fn new(user_agent: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(&user_agent)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            user_agent,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_ttl: 300, // 榛樿5鍒嗛挓缂撳瓨
        }
    }

    /// 设置缓存过期时间
    #[allow(unused)]
    pub fn set_cache_ttl(&mut self, ttl_seconds: u64) {
        self.cache_ttl = ttl_seconds;
    }

    /// 清除缓存
    #[allow(unused)]
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        cache.clear();
    }

    /// 为example://协议生成示例文章
    async fn generate_example_articles(&self, url: &str) -> Vec<Article> {
        let mut articles = Vec::new();

        // 根据URL生成不同内容的示例文章
        let title = match url {
            "example://welcome" => "欢迎使用 Rust RSS 阅读器",
            _ => "示例文章标题",
        };

        // 创建示例文章
        let example_article = Article {
            id: 0, // 将在存储时分配ID
            feed_id: 0, // 将在存储时分配feed_id
            title: title.to_string(),
            link: url.to_string(),
            summary: "这是一篇示例文章，用于测试目的。".to_string(),
            content: "<p>这是示例文章的详细内容。</p><p>Rust RSS 阅读器是一个高性能、轻量级的RSS阅读应用。</p>".to_string(),
            author: "Rust RSS Reader Team".to_string(),
            pub_date: Utc::now(),
            is_read: false,
            is_starred: false,
            guid: format!("{}-{}", url, Utc::now().timestamp()),
            source: url.to_string(), // 添加source字段
        };

        articles.push(example_article);
        articles
    }



    /// 单次获取网页内容
    #[allow(unused)]
    async fn fetch_web_content_once(&self, url: &str) -> Result<String, RssError> {
        // 检查URL是否有效
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(RssError::NetworkError("Invalid URL format".to_string()));
        }

        log::debug!("Fetching web content from: {}", url);

        // 发送HTTP请求，模拟浏览器行为
        let response = self.client.get(url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7")
            .header("Accept-Encoding", "identity") // 明确要求不压缩，避免gzip解压问题
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("Referer", "https://www.google.com/")
            .header("DNT", "1")
            .header("Upgrade-Insecure-Requests", "1")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        // 检查响应状态
        let status = response.status();
        if !status.is_success() {
            // 只返回状态码和简短描述，不返回完整的错误响应内容
            // 对于403错误，特别处理，只显示状态码和简短描述
            let error_msg = match status.as_u16() {
                403 => "HTTP 403 Forbidden - 服务器拒绝访问该网页".to_string(),
                404 => "HTTP 404 Not Found - 网页不存在".to_string(),
                500 => "HTTP 500 Internal Server Error - 服务器内部错误".to_string(),
                503 => "HTTP 503 Service Unavailable - 服务器暂时不可用".to_string(),
                _ => format!("HTTP {} - 请求失败", status),
            };
            return Err(RssError::NetworkError(error_msg));
        }

        // 获取内容
        let content = response.text().await?;
        Ok(content)
    }

    /// 获取并解析RSS源（带重试机制）
    pub async fn fetch_feed(&self, url: &str) -> Result<Vec<Article>, RssError> {
        if let Some(articles) = self.get_from_cache(url).await {
            log::info!("Returning cached articles for {}", url);
            return Ok(articles);
        }

        log::info!("Fetching feed: {}", url);

        // 重试机制
        let max_retries = 3;
        let mut last_error: Option<RssError> = None;

        for attempt in 0..max_retries {
            match self.fetch_feed_once(url).await {
                Ok(articles) => {
                    // 缓存结果（直接传递，避免clone）
                    self.add_to_cache(url, articles.clone()).await;
                    log::info!(
                        "Successfully fetched {} articles from {}",
                        articles.len(),
                        url
                    );
                    return Ok(articles);
                }
                Err(e) => {
                    last_error = Some(e);
                    log::warn!(
                        "Attempt {}/{} failed for {}: {:?}",
                        attempt + 1,
                        max_retries,
                        url,
                        last_error
                    );

                    // 指数退避重试
                    if attempt < max_retries - 1 {
                        let delay_ms = 200 * (2_u64.pow(attempt as u32));
                        log::info!("Retrying after {}ms...", delay_ms);
                        sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        // 所有重试都失败
        Err(last_error.unwrap_or(RssError::Unknown))
    }

    async fn fetch_feed_once(&self, url: &str) -> Result<Vec<Article>, RssError> {
        // 检查URL是否有效
        if !url.starts_with("http://") && !url.starts_with("https://") {
            // 特殊处理example://协议，返回示例文章
            if url.starts_with("example://") {
                log::info!("Handling example protocol URL: {}", url);
                return Ok(self.generate_example_articles(url).await);
            }
            return Err(RssError::NetworkError("Invalid URL format".to_string()));
        }

        log::debug!("Fetching feed from: {}", url);

        // 发送HTTP请求，添加更多的容错处理
        let response = self
            .client
            .get(url)
            .header("User-Agent", &self.user_agent)
            .header(
                "Accept",
                "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
            )
            .header("Referer", url) // 策略1：指向自己（简单有效）
            //.header("Accept-Encoding", "identity") // 明确要求不压缩，避免gzip解压问题
            .header("Cache-Control", "no-cache")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8") // 添加语言支持
            .timeout(std::time::Duration::from_secs(15)) // 增加超时时间
            .send()
            .await?;

        // 获取Content-Type和Content-Encoding
        let content_type_str = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let content_encoding = response
            .headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        // 先获取status
        let status = response.status();

        log::debug!("Content-Encoding: {} for {}", content_encoding, url);

        // 对于错误状态，我们只需要读取文本
        if !status.is_success() {
            // 如果是403错误，尝试使用Headless Chrome获取
            if status.as_u16() == 403 {
                log::warn!("HTTP 403 Forbidden，尝试使用Headless Chrome获取RSS源: {}", url);
                return self.fetch_feed_with_headless_chrome(url).await;
            }
            
            let error_text = response.text().await.unwrap_or_default();
            return Err(RssError::NetworkError(format!(
                "HTTP error: {} - {}",
                status,
                error_text
            )));
        }

        // 对于成功状态，读取字节
        let body = response.bytes().await?;

        // 添加内容完整性检查
        if body.is_empty() {
            log::error!("Response body is empty for URL: {}", url);
            return Err(RssError::NetworkError("服务器返回空内容".to_string()));
        }

        log::debug!("Content-Type: {} for {}", content_type_str, url);

        // 添加详细日志，帮助调试
        log::debug!("Response body length: {} bytes", body.len());

        // 打印前200字节的十六进制表示，用于调试
        let preview_size = std::cmp::min(body.len(), 200);
        log::debug!(
            "First {} bytes of response: {:?}",
            preview_size,
            &body[..preview_size]
        );

        // 检查响应是否包含有效的XML起始标签
        let s = String::from_utf8_lossy(&body);
        let is_valid_xml = s.trim_start().starts_with("<?xml") || s.trim_start().starts_with("<rss") || s.trim_start().starts_with("<feed");

        if !is_valid_xml {
            log::error!("Response does not contain valid XML content for URL: {}", url);
            // 尝试将响应转换为字符串，以便记录
            if let Ok(text) = String::from_utf8(body.to_vec()) {
                log::error!("Invalid XML content preview: {}", text.chars().take(500).collect::<String>());
            }
            return Err(RssError::NetworkError("服务器返回无效的XML内容".to_string()));
        }

        // 检查是否是HTML页面（可能是错误页面或重定向）
        if let Ok(text) = String::from_utf8(body.to_vec()) {
            let lower_text = text.to_lowercase();
            if lower_text.contains("<!doctype html") || lower_text.contains("<html") {
                // 检查是否是404页面
                if lower_text.contains("404") || lower_text.contains("not found") {
                    return Err(RssError::NetworkError(
                        "服务器返回404错误，该RSS源可能不存在或已更改".to_string(),
                    ));
                }
                log::warn!("检测到HTML内容而非RSS，可能是错误页面或重定向");

                // 尝试提取可能的RSS链接（比如link rel="alternate" type="application/rss+xml"）
                if let Some(rss_link) = self.extract_rss_link_from_html(&text) {
                    log::info!("从HTML页面中提取到RSS链接: {}", rss_link);
                    // 返回特殊错误，提示用户使用提取到的RSS链接
                    return Err(RssError::NetworkError(format!(
                        "检测到HTML页面，已提取RSS链接: {}",
                        rss_link
                    )));
                }
            }
        }

        // 1. 尝试直接解析
        match Channel::read_from(&body[..]) {
            Ok(channel) => {
                log::info!("🎉 直接解析成功");
                return Ok(self.parse_items_to_articles(&channel));
            }
            Err(e) => log::error!("Failed to parse RSS content directly: {:?}", e),
        }

        // 2. 尝试移除BOM（Byte Order Mark）
        log::info!("尝试移除BOM后解析");
        if body.len() >= 3 && body[0] == 0xEF && body[1] == 0xBB && body[2] == 0xBF {
            // 移除UTF-8 BOM
            let bomless = &body[3..];
            match Channel::read_from(bomless) {
                Ok(channel) => {
                    log::info!("🎉 移除UTF-8 BOM后解析成功");
                    return Ok(self.parse_items_to_articles(&channel));
                }
                Err(e) => log::error!("移除UTF-8 BOM后解析失败: {:?}", e),
            }
        }

        // 3. 尝试使用UTF-8转换与lossy处理
        log::info!("尝试UTF-8 lossy转换后解析");
        let lossy_text = String::from_utf8_lossy(&body);
        match Channel::read_from(lossy_text.as_bytes()) {
            Ok(channel) => {
                log::info!("🎉 UTF-8 lossy转换后解析成功");
                return Ok(self.parse_items_to_articles(&channel));
            }
            Err(e) => log::error!("UTF-8 lossy转换后解析失败: {:?}", e),
        }

        // 4. 尝试不同的编码（如GBK）
        log::info!("尝试使用GBK编码解析");
        let (text, _encoding, _had_errors) = encoding_rs::GBK.decode(&body);
        match Channel::read_from(text.as_bytes()) {
            Ok(channel) => {
                log::info!("🎉 GBK解码后解析成功");
                return Ok(self.parse_items_to_articles(&channel));
            }
            Err(e) => log::error!("GBK解码后解析失败: {:?}", e),
        }

        // 5. 尝试更多编码
        log::info!("尝试使用UTF-16编码解析");
        if let Ok(utf16_text) = String::from_utf16(&self.try_decode_utf16(&body)) {
            match Channel::read_from(utf16_text.as_bytes()) {
                Ok(channel) => {
                    log::info!("🎉 UTF-16解码后解析成功");
                    return Ok(self.parse_items_to_articles(&channel));
                }
                Err(e) => log::error!("UTF-16解码后解析失败: {:?}", e),
            }
        }

        // 6. 尝试使用GBK编码的另一种实现方式
        log::info!("尝试使用GBK编码的替代实现解析");
        // 导入encoding_rs的编码常量
        use encoding_rs::{GBK, EUC_KR};
        let (text, _encoding, _had_errors) = GBK.decode(&body);
        match Channel::read_from(text.as_bytes()) {
            Ok(channel) => {
                log::info!("🎉 GBK替代实现解码后解析成功");
                return Ok(self.parse_items_to_articles(&channel));
            }
            Err(e) => log::error!("GBK替代实现解码后解析失败: {:?}", e),
        }
        
        // 7. 尝试使用EUC-KR编码（韩文编码）
        log::info!("尝试使用EUC-KR编码解析");
        let (text, _encoding, _had_errors) = EUC_KR.decode(&body);
        let text_bytes = text.as_bytes();
        match Channel::read_from(text_bytes) {
            Ok(channel) => {
                log::info!("🎉 EUC-KR解码后解析成功");
                return Ok(self.parse_items_to_articles(&channel));
            }
            Err(e) => log::error!("EUC-KR解码后解析失败: {:?}", e),
        }

        // 7. 尝试对文本进行清理和预处理
        log::info!("尝试文本清理和预处理后解析");
        // String::from_utf8_lossy直接返回String，不需要Result匹配
        let text = String::from_utf8_lossy(&body).to_string();
        // 移除BOM字符
        let cleaned_text = text.replace("\u{FEFF}", "");
        // 尝试移除HTML标签
        let cleaned_text = regex::Regex::new(r"<[^>]*>")
            .ok().map(|re| re.replace_all(&cleaned_text, "").to_string())
            .unwrap_or(cleaned_text);

        match Channel::read_from(cleaned_text.as_bytes()) {
            Ok(channel) => {
                log::info!("🎉 文本清理后解析成功");
                return Ok(self.parse_items_to_articles(&channel));
            }
            Err(e) => log::error!("文本清理后解析失败: {:?}", e),
        }

        // 5. 尝试解压（如果响应可能被压缩）
        log::info!("尝试解压后解析");
        let should_try_decompress = content_encoding.contains("gzip")
            || content_encoding.contains("br")
            || content_encoding.contains("deflate")
            || body.len() > 100; // 如果内容较长，也尝试解压

        if should_try_decompress {
            log::info!("Attempting to decompress based on Content-Encoding or content length");
            if let Ok(decompressed) = self.try_decompress_gzip(&body) {
                log::info!("Successfully decompressed gzip content");
                match Channel::read_from(&decompressed[..]) {
                    Ok(channel) => {
                        log::info!("🎉 解压后解析成功");
                        return Ok(self.parse_items_to_articles(&channel));
                    }
                    Err(e) => {
                        log::error!("Failed to parse decompressed RSS content: {:?}", e);
                    }
                }
            } else {
                log::error!("解压失败");
            }
        }

        // 在所有RSS解析尝试失败后，尝试检测并解析Atom格式
        log::info!("尝试检测Atom格式并解析");
        // 将body转换为字符串以检测Atom格式
        let text = String::from_utf8_lossy(&body).to_string();
        // 检查是否是Atom格式
        if self.is_atom_format(&text) {
            log::info!("检测到Atom格式，尝试使用Atom解析器");
            // 使用Atom解析器解析
            match self.parse_atom_content(&text, url) {
                Ok(articles) => {
                    log::info!("🎉 Atom解析成功");
                    return Ok(articles);
                }
                Err(e) => {
                    log::error!("Atom解析失败: {:?}", e);
                }
            }
        }

        // 如果所有尝试都失败，则返回错误
        Err(RssError::NetworkError(
            "无法解析RSS/Atom内容，尝试了多种编码和解析方法".to_string(),
        ))
    }

    async fn get_from_cache(&self, url: &str) -> Option<Vec<Article>> {
        let mut cache = self.cache.lock().await;

        if let Some((timestamp, articles)) = cache.get(url) {
            if Instant::now().duration_since(*timestamp).as_secs() < self.cache_ttl {
                return Some(articles.clone());
            } else {
                // 过期了，移除缓存
                cache.remove(url);
            }
        }

        None
    }

    async fn add_to_cache(&self, url: &str, articles: Vec<Article>) {
        let mut cache = self.cache.lock().await;
        cache.insert(url.to_string(), (Instant::now(), articles));

        // 限制缓存大小，移除最早的条目
        const MAX_CACHE_SIZE: usize = 100;
        if cache.len() > MAX_CACHE_SIZE {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, (ts, _))| ts)
                .map(|(key, _)| key.clone());

            if let Some(key) = oldest_key {
                cache.remove(&key);
            }
        }
    }

    /// 使用Headless Chrome获取RSS源
    async fn fetch_feed_with_headless_chrome(&self, url: &str) -> Result<Vec<Article>, RssError> {
        log::info!("尝试使用Headless Chrome获取RSS源: {}", url);
        
        // 构建浏览器启动选项，使用非无头模式以避免403错误
        // 并添加命令行参数，尽量隐藏浏览器窗口
        let launch_options = LaunchOptionsBuilder::default()
            .headless(false) // 禁用无头模式，因为很多网站会阻止无头浏览器
            .window_size(Some((800, 600)))
            // 添加命令行参数，将窗口定位到屏幕外
            .args(vec![
                OsStr::new("--window-position=-32000,-32000"), // 将窗口定位到屏幕外
                OsStr::new("--no-startup-window"), // 不显示启动窗口
                OsStr::new("--silent-launch"), // 静默启动
                OsStr::new("--disable-extensions"), // 禁用扩展
                OsStr::new("--disable-popup-blocking"), // 禁用弹窗阻止
                OsStr::new("--disable-default-apps"), // 禁用默认应用
            ])
            .build()
            .map_err(|e| {
                log::error!("构建浏览器启动选项失败: {:?}", e);
                RssError::NetworkError(format!("Headless Chrome启动失败: {:?}", e))
            })?;

        // 启动浏览器
        let browser = Browser::new(launch_options)
            .map_err(|e| {
                log::error!("启动Headless Chrome失败: {:?}", e);
                RssError::NetworkError(format!("Headless Chrome启动失败: {:?}", e))
            })?;

        // 创建新标签页
        let tab = browser.new_tab()
            .map_err(|e| {
                log::error!("创建新标签页失败: {:?}", e);
                RssError::NetworkError(format!("创建标签页失败: {:?}", e))
            })?;
            

        // 导航到RSS源URL
        tab.navigate_to(url)
            .map_err(|e| {
                log::error!("导航到URL失败: {:?}", e);
                RssError::NetworkError(format!("导航失败: {:?}", e))
            })?;

        // 等待页面加载完成
        tab.wait_until_navigated()
            .map_err(|e| {
                log::error!("等待页面加载失败: {:?}", e);
                RssError::NetworkError(format!("页面加载失败: {:?}", e))
            })?;

        // 获取页面内容
        let page_content = tab.get_content()
            .map_err(|e| {
                log::error!("获取页面内容失败: {:?}", e);
                RssError::NetworkError(format!("获取页面内容失败: {:?}", e))
            })?;

        log::info!("使用Headless Chrome获取到页面内容，长度: {} bytes", page_content.len());
        log::info!("内容预览: {:?}", &page_content[..std::cmp::min(page_content.len(), 200)]);

        // 尝试从HTML中提取RSS内容
        let rss_content = if page_content.starts_with("<?xml") {
            // 如果直接是XML内容，直接使用
            page_content
        } else {
            // 否则尝试从HTML中提取RSS内容
            log::info!("从HTML页面中提取RSS内容");
            
            // 查找pre标签中的内容，这通常包含RSS XML
            let pre_content = if let Some(start) = page_content.find("<pre") {
                if let Some(end_start) = page_content[start..].find(">").map(|i| start + i + 1) {
                    if let Some(end) = page_content[end_start..].find("</pre>").map(|i| end_start + i) {
                        page_content[end_start..end].to_string()
                    } else {
                        log::error!("未找到</pre>标签");
                        return Err(RssError::NetworkError("从HTML中提取RSS内容失败: 未找到完整的pre标签".to_string()));
                    }
                } else {
                    log::error!("未找到<pre>标签的结束符");
                    return Err(RssError::NetworkError("从HTML中提取RSS内容失败: 未找到pre标签结束符".to_string()));
                }
            } else {
                log::error!("未找到<pre>标签");
                return Err(RssError::NetworkError("从HTML中提取RSS内容失败: 未找到pre标签".to_string()));
            };
            
            log::info!("提取到pre标签内容，长度: {} bytes", pre_content.len());
            log::info!("pre内容预览: {:?}", &pre_content[..std::cmp::min(pre_content.len(), 200)]);
            
            // 解码HTML实体，如&lt; -> <
            let decoded_content = html_escape::decode_html_entities(&pre_content).to_string();
            log::info!("解码后内容预览: {:?}", &decoded_content[..std::cmp::min(decoded_content.len(), 200)]);
            
            decoded_content
        };

        // 解析RSS内容
        let channel = Channel::read_from(rss_content.as_bytes())
            .map_err(|e| {
                log::error!("解析RSS内容失败: {:?}", e);
                RssError::ParseError(e)
            })?;

        // 转换为Article类型
        Ok(self.parse_items_to_articles(&channel))
    }

    #[allow(unused)]
    pub async fn fetch_multiple_feeds(
        &self,
        urls: &[&str],
    ) -> HashMap<String, Result<Vec<Article>, RssError>> {
        let mut results = HashMap::new();
        let mut tasks = Vec::new();

        for &url in urls {
            let fetcher = self.clone();
            let url_str = url.to_string();

            // 直接使用url_str，无需额外clone
            let task = tokio::spawn(async move { (url_str.clone(), fetcher.fetch_feed(&url_str).await) });

            tasks.push(task);
        }

        for task in tasks {
            if let Ok((url, result)) = task.await {
                results.insert(url, result);
            }
        }

        results
    }
    
    /// 使用Headless Chrome获取网页内容
    pub async fn fetch_web_content(&self, url: &str) -> Result<String, RssError> {
        log::info!("尝试使用Headless Chrome获取网页内容: {}", url);
        
        // 构建浏览器启动选项，使用非无头模式以避免403错误
        // 并添加命令行参数，尽量隐藏浏览器窗口
        let launch_options = LaunchOptionsBuilder::default()
            .headless(false) // 禁用无头模式，因为很多网站会阻止无头浏览器
            .window_size(Some((800, 600)))
            // 添加命令行参数，将窗口定位到屏幕外
            .args(vec![
                OsStr::new("--window-position=-32000,-32000"), // 将窗口定位到屏幕外
                OsStr::new("--no-startup-window"), // 不显示启动窗口
                OsStr::new("--silent-launch"), // 静默启动
                OsStr::new("--disable-extensions"), // 禁用扩展
                OsStr::new("--disable-popup-blocking"), // 禁用弹窗阻止
                OsStr::new("--disable-default-apps"), // 禁用默认应用
            ])
            .build()
            .map_err(|e| {
                log::error!("构建浏览器启动选项失败: {:?}", e);
                RssError::NetworkError(format!("Headless Chrome启动失败: {:?}", e))
            })?;

        // 启动浏览器
        let browser = Browser::new(launch_options)
            .map_err(|e| {
                log::error!("启动Headless Chrome失败: {:?}", e);
                RssError::NetworkError(format!("Headless Chrome启动失败: {:?}", e))
            })?;

        // 创建新标签页
        let tab = browser.new_tab()
            .map_err(|e| {
                log::error!("创建新标签页失败: {:?}", e);
                RssError::NetworkError(format!("创建标签页失败: {:?}", e))
            })?;
            

        // 导航到URL
        tab.navigate_to(url)
            .map_err(|e| {
                log::error!("导航到URL失败: {:?}", e);
                RssError::NetworkError(format!("导航失败: {:?}", e))
            })?;

        // 等待页面加载完成
        tab.wait_until_navigated()
            .map_err(|e| {
                log::error!("等待页面加载失败: {:?}", e);
                RssError::NetworkError(format!("页面加载失败: {:?}", e))
            })?;

        // 获取页面内容
        let page_content = tab.get_content()
            .map_err(|e| {
                log::error!("获取页面内容失败: {:?}", e);
                RssError::NetworkError(format!("获取页面内容失败: {:?}", e))
            })?;

        log::info!("使用Headless Chrome获取到网页内容，长度: {} bytes", page_content.len());
        log::info!("内容预览: {:?}", &page_content[..std::cmp::min(page_content.len(), 200)]);
        
        Ok(page_content)
    }

    /// 将RSS Item转换为Article类型
    fn parse_items_to_articles(&self, channel: &Channel) -> Vec<Article> {
        let source = channel.title().to_string();

        channel
            .items()
            .iter()
            .map(|item| self.item_to_article(item, &source))
            .collect()
    }

    /// 将单个RSS Item转换为Article
    fn item_to_article(&self, item: &Item, source: &str) -> Article {
        let pub_date = if let Some(date_str) = item.pub_date() {
            match self.parse_pub_date(date_str) {
                Ok(date) => date,
                Err(_) => {
                    log::warn!("Failed to parse date: {}", date_str);
                    Utc::now()
                }
            }
        } else {
            Utc::now()
        };

        let content = self.get_content(item);

        let summary = self.get_summary(item);

        // 获取作者信息，首先尝试标准author字段，然后尝试dc:creator扩展
        let author = item.author()
            .map(|a| a.to_string())
            .or_else(|| {
                // 从Dublin Core扩展中获取dc:creator信息
                item.dublin_core_ext()
                    .and_then(|dc_ext| {
                        dc_ext.creators()
                            .first()
                            .map(|c| c.to_string())
                    })
            })
            .unwrap_or_else(|| "Unknown".to_string());

        Article {
            id: 0,      // 将由数据库自动生成
            feed_id: 0, // 将在添加到数据库时设置
            title: html_escape::decode_html_entities(item.title().unwrap_or("Untitled")).to_string(),
            link: item.link().unwrap_or("").to_string(),
            author,
            pub_date,
            content,
            summary,
            is_read: false,
            is_starred: false,
            source: source.to_string(),
            guid: item.guid().map_or_else(
                || item.link().unwrap_or("").to_string(),
                |guid| guid.value().to_string(),
            ),
        }
    }

    /// 解析发布日期
    fn parse_pub_date(&self, date_str: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
        // 尝试清理日期字符串，移除可能的多余空格
        let trimmed_date_str = date_str.trim();

        // 尝试多种日期格式，特别添加GMT时区格式和更多变体
        let formats = [
            "%a, %d %b %Y %H:%M:%S %z",
            "%a, %d %b %Y %H:%M:%S GMT", // IT之家使用的格式
            "%a,%d %b %Y %H:%M:%S GMT",  // 无空格变体
            "%a, %d %b %Y %H:%M:%S %Z",  // 带时区名称的格式
            "%Y-%m-%dT%H:%M:%S%z",
            "%Y-%m-%d %H:%M:%S",
            "%d %b %Y %H:%M:%S",
            // RFC 2822 格式变体
            "%a, %d %b %Y %H:%M:%S GMT+00:00",
            "%a, %d %b %Y %H:%M:%S +0000",
            // 更多GMT格式变体
            "%a, %d %b %Y %H:%M:%S GMT", // 带逗号的GMT格式
            "%a, %d %b %Y %H:%M:%S %Z",  // 带逗号的时区名称格式
        ];

        // 尝试每种格式，返回第一个成功的结果
        for fmt in &formats {
            if let Ok(date) = DateTime::parse_from_str(trimmed_date_str, fmt) {
                return Ok(date.with_timezone(&Utc));
            }
        }

        // 尝试使用RFC 3339格式
        if let Ok(date) = DateTime::parse_from_rfc3339(trimmed_date_str) {
            return Ok(date.with_timezone(&Utc));
        }

        // 尝试解析没有逗号的GMT格式
        if trimmed_date_str.contains("GMT") && !trimmed_date_str.contains(",")
            && let Ok(date) = DateTime::parse_from_str(trimmed_date_str, "%a %d %b %Y %H:%M:%S GMT")
            {
                return Ok(date.with_timezone(&Utc));
            }

        // 专门处理包含逗号的GMT格式，将GMT替换为+00:00再尝试解析
        if trimmed_date_str.contains(",") && trimmed_date_str.contains("GMT") {
            let modified_date_str = trimmed_date_str.replace("GMT", "+00:00");
            if let Ok(date) =
                DateTime::parse_from_str(&modified_date_str, "%a, %d %b %Y %H:%M:%S %z")
            {
                return Ok(date.with_timezone(&Utc));
            }
        }

        // 所有格式都失败，返回一个错误
        // 由于chrono::ParseError不支持直接创建，我们尝试解析一个无效字符串来获取错误
        DateTime::parse_from_str("invalid-date", "%Y-%m-%d").map(|date| date.with_timezone(&Utc))
    }

    /// 获取文章内容
    fn get_content(&self, item: &Item) -> String {
        // 首先尝试获取content:encoded
        if let Some(content) = item.content() {
            return self.sanitize_content(content);
        }

        // 如果没有，尝试获取description
        if let Some(description) = item.description() {
            return self.sanitize_content(description);
        }

        // 如果都没有，返回空字符串
        String::new()
    }

    /// 获取文章摘要
    fn get_summary(&self, item: &Item) -> String {
        // 首先尝试获取description
        if let Some(description) = item.description() {
            let plain_text = self.html_to_plain_text(description);
            return plain_text.chars().take(200).collect::<String>();
        }

        if let Some(content) = item.content() {
            let plain_text = self.html_to_plain_text(content);
            return plain_text.chars().take(200).collect::<String>();
        }

        // 如果都没有，返回空字符串
        String::new()
    }

    /// 清理HTML内容，移除无用标签，只保留纯文本和必要格式
    fn sanitize_content(&self, html: &str) -> String {
        // 首先使用html2text将HTML转换为纯文本，保留基本格式
        let plain_text = self.html_to_plain_text(html);
        
        // 解码HTML实体
        let decoded = html_escape::decode_html_entities(&plain_text);
        
        // 移除多余的空白字符，保留合理的换行和空格
        
        
        decoded
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 提取文章中的图片
    #[allow(unused)]
    pub fn extract_images(&self, html: &str) -> Vec<String> {
        let mut images = Vec::new();

        // 绠€鍗曠殑鍥剧墖鎻愬彇閫昏緫
        for part in html.split("<img") {
            if let Some(start) = part.find("src=") {
                let src_part = &part[start + 5..];
                if let Some(end) = src_part.find('"') {
                    images.push(src_part[..end].to_string());
                }
            }
        }

        images
    }

    /// 获取订阅源图标URL
    #[allow(unused)]
    pub async fn get_favicon_url(&self, feed_url: &str) -> Option<String> {
        // 尝试从域名获取favicon
        if let Some(domain) = self.extract_domain(feed_url) {
            let favicon_url = format!("https://{}/favicon.ico", domain);

            // 检测favicon是否存在
            if let Ok(response) = self.client.head(&favicon_url).send().await
                && response.status().is_success() {
                    return Some(favicon_url);
                }
        }

        None
    }

    /// 从URL提取域名
    #[allow(unused)]
    fn extract_domain(&self, url: &str) -> Option<String> {
        // 简单的域名提取逻辑
        if let Some(start) = url.find("://") {
            let remaining = &url[start + 3..];
            if let Some(end) = remaining.find('/') {
                return Some(remaining[..end].to_string());
            } else {
                return Some(remaining.to_string());
            }
        }

        None
    }

    /// 尝试手动解压gzip内容作为备用方案，更加灵活地处理各种压缩格式
    fn try_decompress_gzip(&self, bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        use std::io::Read;

        // 即使没有标准gzip头部也尝试解压，因为IT之家RSS源可能使用非标准压缩
        log::info!("Attempting to manually decompress content, even without standard gzip header");

        // 检查是否有gzip头部标识（1F 8B）
        if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
            log::debug!("检测到标准gzip头部，尝试标准解压");

            // 尝试gzip解压
            let mut decoder = GzDecoder::new(bytes);
            let mut decompressed = Vec::new();
            match decoder.read_to_end(&mut decompressed) {
                Ok(_) => {
                    log::debug!(
                        "Successfully decompressed with gzip decoder, size: {} bytes",
                        decompressed.len()
                    );
                    return Ok(decompressed);
                }
                Err(e) => {
                    log::warn!("Gzip decompression failed: {}, trying fallback methods", e);
                }
            }
        }

        // 尝试使用flate2的低级别API
        let mut decoder = flate2::read::MultiGzDecoder::new(bytes);
        let mut decompressed = Vec::new();
        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => {
                log::debug!(
                    "Successfully decompressed with MultiGzDecoder, size: {} bytes",
                    decompressed.len()
                );
                return Ok(decompressed);
            }
            Err(e) => {
                log::warn!("MultiGzDecoder decompression failed: {}", e);
            }
        }

        // 尝试使用deflate解码器
        log::info!("尝试使用deflate解码器");
        let mut decoder = flate2::read::DeflateDecoder::new(bytes);
        let mut decompressed = Vec::new();
        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => {
                log::debug!(
                    "Successfully decompressed with DeflateDecoder, size: {} bytes",
                    decompressed.len()
                );
                return Ok(decompressed);
            }
            Err(e) => {
                log::warn!("DeflateDecoder decompression failed: {}", e);
            }
        }

        // 所有解压方法都失败
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Failed to decompress data with all available methods",
        ))
    }

    /// 尝试从HTML页面中提取RSS链接
    fn extract_rss_link_from_html(&self, html: &str) -> Option<String> {
        // 查找类似 <link rel="alternate" type="application/rss+xml" href="..."> 的标签
        if let Ok(pattern) = regex::Regex::new(
            r#"<link[^>]+rel=['"]alternate['"].*?type=['"]application/rss\+xml['"].*?href=['"]([^'"]+)['"]"#,
        )
            && let Some(caps) = pattern.captures(html) {
                return Some(caps[1].to_string());
            }

        // 查找类似 <link rel="alternate" type="application/atom+xml" href="..."> 的标签
        if let Ok(pattern) = regex::Regex::new(
            r#"<link[^>]+rel=['"]alternate['"].*?type=['"]application/atom\+xml['"].*?href=['"]([^'"]+)['"]"#,
        )
            && let Some(caps) = pattern.captures(html) {
                return Some(caps[1].to_string());
            }

        None
    }

    /// 尝试将字节数组解码为UTF-16
    fn try_decode_utf16(&self, bytes: &[u8]) -> Vec<u16> {
        let mut result = Vec::new();

        // 检查是否有BOM
        let is_be = bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF;

        // 跳过BOM
        let start_idx = if bytes.len() >= 2
            && (bytes[0] == 0xFE && bytes[1] == 0xFF || bytes[0] == 0xFF && bytes[1] == 0xFE)
        {
            2
        } else {
            0
        };

        // 解码剩余的字节
        let mut i = start_idx;
        while i + 1 < bytes.len() {
            let value = if is_be {
                ((bytes[i] as u16) << 8) | (bytes[i + 1] as u16)
            } else {
                ((bytes[i + 1] as u16) << 8) | (bytes[i] as u16)
            };
            result.push(value);
            i += 2;
        }

        result
    }

    /// 提取链接URL，确保是有效的URL格式
    #[allow(unused)]
    pub fn extract_link_url(&self, url: &str, base_url: &Option<String>) -> String {
        // 如果URL已经是完整的，则直接返回
        if let Ok(parsed) = Url::parse(url)
            && parsed.scheme() != "" && parsed.domain().is_some() {
                return url.to_string();
            }

        // 如果有基础URL，尝试构建完整URL
        if let Some(base) = base_url
            && let Ok(base_parsed) = Url::parse(base)
                && let Ok(joined) = base_parsed.join(url) {
                    return joined.to_string();
                }

        // 无法构建有效URL，返回原始值
        url.to_string()
    }

    /// 将HTML转换为纯文本
    fn html_to_plain_text(&self, html: &str) -> String {
        let cursor = Cursor::new(html);
        // 处理from_read可能返回的错误，如果出错则返回原始HTML字符串
        match from_read(cursor, 80) {
            Ok(plain_text) => plain_text,
            Err(_) => html.to_string(), // 出错时返回原始HTML
        }
    }

    /// 从OPML字符串导入订阅源
    #[allow(unused)]
    pub async fn import_from_string(
        &self,
        opml_content: &str,
    ) -> Result<Vec<(String, String, String)>, anyhow::Error> {
        let document = roxmltree::Document::parse(opml_content)?;

        let mut feeds = Vec::new();

        // 寻找所有outline元素
        for outline in document
            .descendants()
            .filter(|n| n.tag_name().name() == "outline")
        {
            if let Some(type_attr) = outline.attribute("type") {
                if (type_attr == "rss" || type_attr == "atom")
                    && let (Some(title), Some(url)) = (
                        outline.attribute("title").or(outline.attribute("text")),
                        outline.attribute("xmlUrl"),
                    ) {
                        // 获取层级分组名称
                        let group = if let Some(parent) = outline.parent() {
                            if parent.tag_name().name() == "outline" {
                                parent
                                    .attribute("title")
                                    .or(parent.attribute("text"))
                                    .unwrap_or("")
                                    .to_string()
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };

                        feeds.push((title.to_string(), url.to_string(), group));
                    }
            } else if outline.attribute("xmlUrl").is_some()
                && let (Some(title), Some(url)) = (
                    outline.attribute("title").or(outline.attribute("text")),
                    outline.attribute("xmlUrl"),
                ) {
                    // 鑾峰彇鐖剁骇鍒嗙粍鍚嶇О
                    let group = if let Some(parent) = outline.parent() {
                        if parent.tag_name().name() == "outline" {
                            parent
                                .attribute("title")
                                .or(parent.attribute("text"))
                                .unwrap_or("")
                                .to_string()
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };

                    feeds.push((title.to_string(), url.to_string(), group));
                }
        }

        Ok(feeds)
    }

    // 注意：import_and_validate方法应该在OpmlHandler中

    /// 生成OPML内容
    #[allow(unused)]
    pub async fn generate_opml(&self, feeds: &[(String, String, String)]) -> String {
        let mut opml = String::new();

        // 写入OPML头部
        opml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?><opml version=\"1.0\">");
        opml.push_str("<head>");
        opml.push_str("<title>Rust RSS Reader Subscriptions</title>");
        opml.push_str("<dateCreated>");
        opml.push_str(&Utc::now().to_rfc3339().to_string());
        opml.push_str("</dateCreated>");
        opml.push_str("<ownerName>Rust RSS Reader</ownerName>");
        opml.push_str("</head>");
        opml.push_str("<body>");

        // 按分组组织订阅源
        let mut groups = std::collections::HashMap::new();
        for (title, url, group) in feeds {
            groups
                .entry(group.clone())
                .or_insert_with(Vec::new)
                .push((title, url));
        }

        if let Some(no_group_feeds) = groups.get("") {
            for (title, url) in no_group_feeds {
                opml.push_str(&format!(
                    "<outline type=\"rss\" title=\"{}\" text=\"{}\" xmlUrl=\"{}\"/>",
                    html_escape::encode_text(title),
                    html_escape::encode_text(title),
                    html_escape::encode_text(url)
                ));
            }
        }

        for (group_name, feeds) in groups {
            if !group_name.is_empty() {
                opml.push_str(&format!(
                    "<outline text=\"{}\" title=\"{}\">\n",
                    html_escape::encode_text(&group_name),
                    html_escape::encode_text(&group_name)
                ));

                for (title, url) in feeds {
                    opml.push_str(&format!(
                        "  <outline type=\"rss\" title=\"{}\" text=\"{}\" xmlUrl=\"{}\"/>",
                        html_escape::encode_text(title),
                        html_escape::encode_text(title),
                        html_escape::encode_text(url)
                    ));
                }

                opml.push_str("</outline>");
            }
        }

        // 写入OPML尾部
        opml.push_str("</body></opml>");

        opml
    }

    /// 从OPML文件导入订阅源
    #[allow(unused)]
    pub async fn import_from_file<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> Result<Vec<(String, String, String)>, anyhow::Error> {
        let opml_content = tokio::fs::read_to_string(path).await?;
        let document = roxmltree::Document::parse(&opml_content)?;

        let mut feeds = Vec::new();

        // 寻找所有outline元素
        for outline in document
            .descendants()
            .filter(|n| n.tag_name().name() == "outline")
        {
            if let Some(type_attr) = outline.attribute("type")
                && (type_attr == "rss" || type_attr == "atom")
                && let (Some(title), Some(url)) = (
                    outline.attribute("title").or(outline.attribute("text")),
                    outline.attribute("xmlUrl"),
                ) {
                    // 获取层级分组名称
                    let group = if let Some(parent) = outline.parent() {
                        parent
                            .attribute("title")
                            .or(parent.attribute("text"))
                            .unwrap_or("")
                            .to_string()
                    } else {
                        String::new()
                    };

                    feeds.push((title.to_string(), url.to_string(), group));
            }
        }

        Ok(feeds)
    }

    /// 检测内容是否为Atom格式
    fn is_atom_format(&self, content: &str) -> bool {
        // 检查是否包含Atom命名空间
        content.contains("xmlns=\"http://www.w3.org/2005/Atom\"") || 
        // 检查是否包含Atom特有的元素
        (content.contains("<feed") && content.contains("</feed>") && content.contains("<entry") && content.contains("</entry>"))
    }

    /// 解析Atom格式的内容，提取文章信息
    fn parse_atom_content(&self, content: &str, feed_url: &str) -> Result<Vec<Article>, RssError> {
        log::info!("尝试解析Atom格式内容");

        // 使用roxmltree解析XML/Atom
        let doc = match Document::parse(content) {
            Ok(doc) => doc,
            Err(e) => {
                log::error!("XML解析错误: {}", e);
                return Err(RssError::XmlError(e.to_string()));
            }
        };

        let root = doc.root_element();
        let mut articles = Vec::new();

        // 查找所有文章条目
        let entries: Vec<_> = root
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "entry")
            .collect();

        log::info!("找到 {} 个文章条目", entries.len());

        // 解析每个文章条目
        for entry in entries {
            // 提取标题
            let title = entry
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "title")
                .and_then(|n| n.text())
                .filter(|t| !t.trim().is_empty())
                .map(|t| html_escape::decode_html_entities(t.trim()).to_string())
                .unwrap_or_else(|| "[无标题]".to_string());

            // 提取链接
            let link = entry
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "link")
                .find_map(|n| n.attribute("href"))
                .unwrap_or("")
                .to_string();

            // 提取发布时间，优先使用published，然后是updated
            let pub_date = entry
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "published")
                .or_else(|| {
                    entry
                        .descendants()
                        .find(|n| n.is_element() && n.tag_name().name() == "updated")
                })
                .and_then(|n| n.text())
                .map(|text| match DateTime::parse_from_rfc3339(text.trim()) {
                    Ok(dt) => dt.with_timezone(&Utc),
                    Err(e) => {
                        log::warn!("解析日期失败: {}, 使用当前时间", e);
                        Utc::now()
                    }
                })
                .unwrap_or_else(Utc::now);

            // 提取摘要
            let summary = entry
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "summary")
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
                .unwrap_or_else(String::new);

            // 提取内容
            let content_str = entry
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "content")
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|| summary.clone());
            
            // 清理内容，移除无用标签
            let cleaned_content = self.sanitize_content(&content_str);
            let cleaned_summary = if !summary.is_empty() && summary != content_str {
                self.html_to_plain_text(&summary).chars().take(200).collect::<String>()
            } else {
                String::new()
            };

            // 提取作者
            let author = entry
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "author")
                .and_then(|author_elem| {
                    author_elem
                        .descendants()
                        .find(|n| n.is_element() && n.tag_name().name() == "name")
                        .and_then(|n| n.text())
                        .map(|t| t.trim().to_string())
                })
                .unwrap_or_else(String::new);

            // 提取GUID或使用链接作为GUID
            let guid = entry
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "id")
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|| link.clone());

            // 创建文章对象
            let article = Article {
                id: 0,      // 将由数据库自动生成
                feed_id: 0, // 将在添加到数据库时设置
                title,
                link,
                author,
                pub_date,
                content: cleaned_content,
                summary: cleaned_summary,
                is_read: false,
                is_starred: false,
                source: feed_url.to_string(), // 使用feed_url作为source
                guid,
            };

            articles.push(article);
        }

        if articles.is_empty() {
            log::warn!("Atom解析成功但未找到任何文章");
            return Err(RssError::AtomParseError);
        }

        log::info!("成功解析Atom格式，共找到 {} 篇文章", articles.len());
        Ok(articles)
    }
}

impl Clone for RssFetcher {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            user_agent: self.user_agent.clone(),
            cache: self.cache.clone(),
            cache_ttl: self.cache_ttl,
        }
    }
}

/// OPML导入导出功能
pub struct OpmlHandler {
    fetcher: Option<RssFetcher>,
}

impl OpmlHandler {
    #[allow(unused)]
    pub fn new() -> Self {
        Self { fetcher: None }
    }

    /// 验证订阅源是否有效
    #[allow(unused)]
    pub async fn validate_feeds(
        &self,
        feeds: &[(String, String, String)],
    ) -> Result<Vec<(String, String, String, bool)>, anyhow::Error> {
        if let Some(fetcher) = &self.fetcher {
            let mut validated_feeds = Vec::new();

            for (title, url, group) in feeds {
                // 验证URL是否有效
                let is_valid = match fetcher.fetch_feed(url).await {
                    Ok(_) => true,
                    Err(e) => {
                        log::warn!("Invalid feed {} ({}): {:?}", title, url, e);
                        false
                    }
                };

                validated_feeds.push((title.clone(), url.clone(), group.clone(), is_valid));
            }

            Ok(validated_feeds)
        } else {
            Err(anyhow::anyhow!("Fetcher not initialized"))
        }
    }

    #[allow(unused)]
    pub fn with_fetcher(user_agent: String) -> Self {
        Self {
            fetcher: Some(RssFetcher::new(user_agent)),
        }
    }

    #[allow(unused)]
    pub fn set_fetcher(&mut self, user_agent: String) {
        self.fetcher = Some(RssFetcher::new(user_agent));
    }
    #[allow(unused)]
    pub async fn import_from_file<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> Result<Vec<(String, String, String)>, anyhow::Error> {
        let opml_content = tokio::fs::read_to_string(path).await?;
        let document = roxmltree::Document::parse(&opml_content)?;

        let mut feeds = Vec::new();

        // 鏌ユ壘鎵€鏈塷utline鍏冪礌
        for outline in document
            .descendants()
            .filter(|n| n.tag_name().name() == "outline")
        {
            if let Some(type_attr) = outline.attribute("type")
                && (type_attr == "rss" || type_attr == "atom")
                && let (Some(title), Some(url)) = (
                    outline.attribute("title").or(outline.attribute("text")),
                    outline.attribute("xmlUrl"),
                ) {
                    // 鑾峰彇鐖剁骇鍒嗙粍鍚嶇О
                    let group = if let Some(parent) = outline.parent() {
                        parent
                            .attribute("title")
                            .or(parent.attribute("text"))
                            .unwrap_or("")
                            .to_string()
                    } else {
                        String::new()
                    };

                    feeds.push((title.to_string(), url.to_string(), group));
            }
        }

        Ok(feeds)
    }

    /// 导出订阅源为OPML文件
    #[allow(unused)]
    pub async fn export_to_file<P: AsRef<std::path::Path>>(
        &self,
        path: P,
        feeds: &[(String, String, String)],
    ) -> Result<(), anyhow::Error> {
        let mut opml = String::new();

        // 写入OPML头部
        opml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?><opml version=\"1.0\">");
        opml.push_str("<head><title>Rust RSS Reader Subscriptions</title></head>");
        opml.push_str("<body>");

        // 按分组组织订阅源
        let mut groups = std::collections::HashMap::new();
        for (title, url, group) in feeds {
            groups
                .entry(group.clone())
                .or_insert_with(Vec::new)
                .push((title, url));
        }

        if let Some(no_group_feeds) = groups.get("") {
            for (title, url) in no_group_feeds {
                opml.push_str(&format!(
                    "<outline type=\"rss\" title=\"{}\" xmlUrl=\"{}\"/>",
                    html_escape::encode_text(title),
                    html_escape::encode_text(url)
                ));
            }
        }

        for (group_name, feeds) in groups {
            if !group_name.is_empty() {
                opml.push_str(&format!(
                    "<outline text=\"{}\" title=\"{}\">\n",
                    html_escape::encode_text(&group_name),
                    html_escape::encode_text(&group_name)
                ));

                for (title, url) in feeds {
                    opml.push_str(&format!(
                        "  <outline type=\"rss\" title=\"{}\" xmlUrl=\"{}\"/>",
                        html_escape::encode_text(title),
                        html_escape::encode_text(url)
                    ));
                }

                opml.push_str("</outline>");
            }
        }

        // 写入OPML尾部
        opml.push_str("</body></opml>");

        // 写入文件
        tokio::fs::write(path, opml).await?;

        Ok(())
    }
}
