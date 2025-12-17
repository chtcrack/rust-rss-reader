// 配置管理模块

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

// 导入加密模块
use crate::crypto::{CryptoManager, CryptoError};

/// 应用程序名称（用于数据存储路径）
pub const APP_NAME: &str = "RustRssReader";

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
        let converted = *utc_time + chrono::Duration::hours(8);
        format!("{} (+08:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    } else if timezone_str == "UTC" {
        // 如果是UTC时区，直接返回原始时间
        format!("{} (UTC)", utc_time.format("%Y-%m-%d %H:%M:%S"))
    } else if timezone_str == "Asia/Tokyo" {
        // 如果是Asia/Tokyo
        let converted = *utc_time + chrono::Duration::hours(9);
        format!("{} (+09:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    } else if timezone_str == "America/New_York" {
        // 如果是America/New_York
        let converted = *utc_time + chrono::Duration::hours(-5);
        format!("{} (-05:00)", converted.format("%Y-%m-%d %H:%M:%S"))
    } else {
        // 如果时区解析失败，返回UTC+8时间
        let converted = *utc_time + chrono::Duration::hours(8);
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

    /// AI API Key（加密存储）
    pub ai_api_key: String,

    /// 标记AI API Key是否已加密
    pub ai_api_key_encrypted: bool,

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
        let db_path = "./feed.duckdb".to_string();

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
            // 默认使用直接搜索
            search_mode: "direct_search".to_string(),
            // AI配置默认值
            ai_api_url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
            ai_api_key: "".to_string(),
            ai_api_key_encrypted: false,
            ai_model_name: "Qwen/Qwen3-8B".to_string(),
        }
    }

    /// 加载配置或使用默认值
    pub fn load_or_default() -> Self {
        let path = Self::config_path();

        if let Ok(mut file) = File::open(path) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() {
                let config: Self = match serde_json::from_str::<Self>(&contents) {
                    Ok(mut config) => {
                        // 检查并更新数据库路径，确保使用正确的文件名
                        if config.database_path.ends_with("feeds.db") || config.database_path.ends_with("feed.db") {
                            config.database_path = "./feed.duckdb".to_string();
                            // 保存更新后的配置
                            config.save().ok();
                        }
                        config
                    },
                    Err(_) => {
                        // 解析失败，返回默认配置
                        return Self::default();
                    }
                };
                return config;
            }
        }

        // 如果加载失败，返回默认配置
        Self::default()
    }

    /// 保存配置到文件
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::config_path();
        
        // 创建一个临时配置，用于保存到文件
        // 如果API密钥未加密，加密后再保存
        let mut config_to_save = self.clone();
        
        // 只在API密钥不为空且未加密的情况下进行加密
        if !config_to_save.ai_api_key.is_empty() && !config_to_save.ai_api_key_encrypted {
            match CryptoManager::new() {
                Ok(crypto_manager) => {
                    match crypto_manager.encrypt(&config_to_save.ai_api_key) {
                    Ok(encrypted_key) => {
                        config_to_save.ai_api_key = encrypted_key;
                        config_to_save.ai_api_key_encrypted = true;
                    },
                    Err(e) => {
                        log::error!("保存配置时加密API密钥失败: {:?}", e);
                        // 加密失败，继续保存，但标记为未加密
                        config_to_save.ai_api_key_encrypted = false;
                    },
                }
                },
                Err(e) => {
                    log::error!("创建加密管理器失败: {:?}", e);
                    // 创建加密管理器失败，继续保存，但标记为未加密
                    config_to_save.ai_api_key_encrypted = false;
                },
            }
        }

        let json = serde_json::to_string_pretty(&config_to_save)?;
        let mut file = File::create(path)?;
        file.write_all(json.as_bytes())?;

        Ok(())
    }
    
    /// 获取解密后的API密钥
    pub fn get_decrypted_api_key(&self) -> Result<String, CryptoError> {
        if self.ai_api_key.is_empty() {
            return Ok("".to_string());
        }
        
        // 创建加密管理器
        let crypto_manager = CryptoManager::new()?;
        
        // 检查API密钥是否已加密，即使标记为未加密，也检查格式
        let is_encrypted = self.ai_api_key_encrypted || crypto_manager.is_encrypted(&self.ai_api_key);
        
        if !is_encrypted {
            return Ok(self.ai_api_key.clone());
        }
        
        crypto_manager.decrypt(&self.ai_api_key)
    }
    
    /// 设置API密钥（自动处理加密）
    #[allow(dead_code)]
    pub fn set_api_key(&mut self, api_key: &str) {
        // 清除加密标记，因为我们设置的是明文
        self.ai_api_key = api_key.to_string();
        self.ai_api_key_encrypted = false;
    }
    
    /// 加密API密钥（如果未加密）
    #[allow(dead_code)]
    pub fn encrypt_api_key(&mut self) -> Result<(), CryptoError> {
        if self.ai_api_key.is_empty() || self.ai_api_key_encrypted {
            return Ok(());
        }
        
        let crypto_manager = CryptoManager::new()?;
        let encrypted_key = crypto_manager.encrypt(&self.ai_api_key)?;
        
        self.ai_api_key = encrypted_key;
        self.ai_api_key_encrypted = true;
        
        Ok(())
    }
    
    /// 解密API密钥（如果已加密）
    #[allow(dead_code)]
    pub fn decrypt_api_key(&mut self) -> Result<(), CryptoError> {
        if self.ai_api_key.is_empty() || !self.ai_api_key_encrypted {
            return Ok(());
        }
        
        let crypto_manager = CryptoManager::new()?;
        let decrypted_key = crypto_manager.decrypt(&self.ai_api_key)?;
        
        self.ai_api_key = decrypted_key;
        self.ai_api_key_encrypted = false;
        
        Ok(())
    }
}
