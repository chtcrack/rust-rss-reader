// 配置管理模块

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

/// 应用程序窗口标题
pub const APP_WINDOW_TITLE: &str = "Rust语言编写的RSS阅读器,代码编写->人工智能,设计思路->Chtcrack";

/// 将UTC时间转换为配置的时区时间并返回格式化的字符串
pub fn convert_to_configured_timezone(utc_time: &DateTime<Utc>, timezone_str: &str) -> String {
    // 尝试解析时区字符串
    if let Ok(timezone) = timezone_str.parse::<Tz>() {
        // 将UTC时间转换为目标时区并格式化
        let converted = utc_time.with_timezone(&timezone);
        format!("{}", converted)
    } else if timezone_str == "Asia/Shanghai" {
        // Asia/Shanghai 特殊处理UTC+8时区
        let converted = utc_time.clone() + chrono::Duration::hours(8);
        format!("{} (+08:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    } else if timezone_str == "UTC" {
        // 如果是UTC时区，直接返回原始时间
        format!("{} (UTC)", utc_time.format("%Y-%m-%d %H:%M:%S"))
    } else if timezone_str == "Asia/Tokyo" {
        // 如果是Asia/Tokyo
        let converted = utc_time.clone() + chrono::Duration::hours(9);
        format!("{} (+09:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    } else if timezone_str == "America/New_York" {
        // 如果是America/New_York
        let converted = utc_time.clone() + chrono::Duration::hours(-5);
        format!("{} (-05:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    } else {
        // 如果时区解析失败，返回UTC+8时间
        let converted = utc_time.clone() + chrono::Duration::hours(8);
        format!("{} (+08:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    }
}

/// 应用程序配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppConfig {
    /// 数据库路径
    pub database_path: String,

    /// 主题设置 (light, dark, system)
    pub theme: String,

    /// 自动刷新间隔（分钟）
    pub auto_refresh_interval: u32,

    /// 用户代理
    pub user_agent: String,

    /// 字体大小
    pub font_size: f32,

    /// 窗口大小
    pub window_width: u32,
    pub window_height: u32,

    /// 是否显示系统托盘图标
    pub show_tray_icon: bool,

    /// 是否启用桌面通知
    pub enable_notifications: bool,

    /// 最大通知数量
    pub max_notifications: usize,

    /// 通知超时时间（毫秒）
    pub notification_timeout_ms: u64,

    /// 时区设置
    pub timezone: String,

    /// 是否显示控制台窗口（仅Windows）
    pub show_console: bool,

    /// 搜索方式设置 (index_search, direct_search)
    pub search_mode: String,

    /// AI API URL地址
    pub ai_api_url: String,

    /// AI API Key
    pub ai_api_key: String,

    /// AI 模型名称
    pub ai_model_name: String,
}

impl AppConfig {
    /// 获取配置文件路径
    fn config_path() -> PathBuf {
        let mut path = if cfg!(target_os = "windows") {
            dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."))
        } else {
            dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
        };

        path.push("rust_rss_reader");

        // 确保目录存在
        std::fs::create_dir_all(&path).ok();

        path.push("config.json");
        path
    }

    /// 创建默认配置
    pub fn default() -> Self {
        let db_path = if cfg!(target_os = "windows") {
            "./feeds.db".to_string()
        } else {
            "./feeds.db".to_string()
        };

        Self {
            database_path: db_path,
            theme: "system".to_string(),
            auto_refresh_interval: 30,
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36".to_string(),
            font_size: 14.0,
            window_width: 1200,
            window_height: 800,
            // 默认禁用系统托盘，以避免潜在的兼容性问题
            show_tray_icon: false,
            enable_notifications: true,
            max_notifications: 3,
            notification_timeout_ms: 5000,
            timezone: "UTC".to_string(), // 时区设置，默认为UTC
            // 默认显示控制台窗口，方便用户查看日志和调试信息
            show_console: true,
            // 默认使用索引搜索
            search_mode: "index_search".to_string(),
            // AI配置默认值
            ai_api_url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
            ai_api_key: "".to_string(),
            ai_model_name: "Qwen/Qwen3-8B".to_string(),
        }
    }

    /// 加载配置或使用默认值
    pub fn load_or_default() -> Self {
        let path = Self::config_path();

        if let Ok(mut file) = File::open(path) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() {
                if let Ok(config) = serde_json::from_str(&contents) {
                    return config;
                }
            }
        }

        // 如果加载失败，返回默认配置
        Self::default()
    }

    /// 保存配置到文件
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::config_path();

        let json = serde_json::to_string_pretty(self)?;
        let mut file = File::create(path)?;
        file.write_all(json.as_bytes())?;

        Ok(())
    }
}
