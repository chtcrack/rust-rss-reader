// 工具函数模块

use chrono::{DateTime, Datelike, Local, Utc};
use reqwest::Url;
use std::path::PathBuf;

/// 格式化日期时间
#[allow(unused)]
pub fn format_datetime(dt: &DateTime<Utc>) -> String {
    let local = dt.with_timezone(&Local);
    let now = Local::now();
    let diff = now.signed_duration_since(local);

    // 根据时间差显示不同的格式
    if diff.num_days() == 0 {
        // 今天的时间，只显示时分
        local.format("%H:%M").to_string()
    } else if diff.num_days() == 1 {
        // 昨天
        "昨天 ".to_string() + &local.format("%H:%M").to_string()
    } else if diff.num_weeks() == 0 {
        // 一周内，显示星期和时间
        format!(
            "{} {}",
            match local.weekday() {
                chrono::Weekday::Mon => "周一",
                chrono::Weekday::Tue => "周二",
                chrono::Weekday::Wed => "周三",
                chrono::Weekday::Thu => "周四",
                chrono::Weekday::Fri => "周五",
                chrono::Weekday::Sat => "周六",
                chrono::Weekday::Sun => "周日",
            },
            local.format("%H:%M")
        )
    } else if diff.num_days() < 30 {
        // 一个月内，显示日期和时间
        local.format("%m-%d %H:%M").to_string()
    } else {
        // 超过一个月，显示完整日期
        local.format("%Y-%m-%d").to_string()
    }
}

/// 从URL获取域名
#[allow(unused)]
pub fn get_domain_from_url(url: &str) -> Option<String> {
    if let Ok(uri) = Url::parse(url)
        && let Some(domain) = uri.domain() {
            return Some(domain.to_string());
        }
    None
}

/// 生成唯一ID
#[allow(unused)]
pub fn generate_unique_id() -> String {
    format!(
        "{}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or(0),
        rand::random::<u32>()
    )
}

/// 获取应用数据目录
#[allow(unused)]
pub fn get_app_data_dir() -> PathBuf {
    if cfg!(target_os = "windows")
        && let Some(mut path) = dirs::data_dir() {
            path.push("rust_rss_reader");
            // 确保目录存在
            if let Err(e) = std::fs::create_dir_all(&path) {
                log::error!("Failed to create app data directory: {}", e);
                return PathBuf::from(".");
            }
            return path;
        }

    // 默认返回当前目录
    PathBuf::from(".")
}

/// 获取缓存目录
#[allow(unused)]
pub fn get_cache_dir() -> PathBuf {
    if cfg!(target_os = "windows")
        && let Some(mut path) = dirs::cache_dir() {
            path.push("rust_rss_reader");
            // 确保目录存在
            if let Err(e) = std::fs::create_dir_all(&path) {
                log::error!("Failed to create cache directory: {}", e);
                return PathBuf::from(".");
            }
            return path;
        }

    // 默认返回当前目录
    PathBuf::from(".")
}

/// 验证URL格式是否有效
#[allow(unused)]
pub fn is_valid_url(url: &str) -> bool {
    Url::parse(url).is_ok()
}

/// 清理HTML标签，只保留纯文本
#[allow(unused)]
pub fn clean_html(html: &str) -> String {
    // 使用正则表达式移除HTML标签
    let re = regex::Regex::new(r"<[^>]*>").expect("Failed to create regex for HTML cleaning");

    let cleaned = re.replace_all(html, "");

    // 解码HTML实体
    html_escape::decode_html_entities(&cleaned).to_string()
}

// 以下函数暂时未使用，保留注释以方便未来使用
/*
/// 截断文本，添加省略号
pub fn truncate_text(text: &str, max_length: usize) -> String {
    if text.len() <= max_length {
        return text.to_string();
    }

    let mut result = text.chars().take(max_length).collect::<String>();
    result.push_str("...");
    result
}

/// 打开外部链接
pub fn open_url(url: &str) -> Result<(), anyhow::Error> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .output()?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .output()?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .output()?;
    }

    Ok(())
}

/// 显示桌面通知
#[cfg(feature = "tray")]
pub fn show_notification(title: &str, body: &str) -> Result<(), anyhow::Error> {
    use notify_rust::Notification;

    Notification::new()
        .summary(title)
        .body(body)
        .show()?;

    Ok(())
}

/// 批量操作时的进度回调
pub type ProgressCallback = Box<dyn Fn(u8) + Send>;

/// 创建默认进度回调
pub fn create_default_progress_callback() -> ProgressCallback {
    Box::new(|progress| {
        log::info!("Operation progress: {}%", progress);
    })
}
*/

/* 暂时注释，未使用
pub fn calculate_reading_time(text: &str) -> u32 {
    // 假设平均阅读速度为每分钟200字
    const WORDS_PER_MINUTE: u32 = 200;

    // 简单地按空格分割计算单词数
    let word_count = text.split_whitespace().count() as u32;

    // 向上取整到最近的分钟
    (word_count + WORDS_PER_MINUTE - 1) / WORDS_PER_MINUTE
}
*/


