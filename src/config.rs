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

/// AI配置项
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AIConfig {
    pub id: String,
    pub name: String,
    pub api_url: String,
    pub api_key: String,
    pub api_key_encrypted: bool,
    pub model_name: String,
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

    /// 是否启用自动清理旧文章
    pub enable_auto_cleanup: bool,

    /// 文章保留天数，超过此天数的文章将被自动清理
    pub article_retention_days: u32,

    /// 每个订阅源保留的最大文章数，超过此数量的旧文章将被自动清理
    pub max_articles_per_feed: u32,

    /// AI配置列表
    pub ai_configs: Vec<AIConfig>,
    /// 当前使用的AI配置ID
    pub current_ai_config_id: String,

    /// 以下字段用于兼容旧版本配置，将在加载时转换为AIConfig
    #[serde(skip_serializing, default = "Default::default")]
    pub ai_api_url: String,
    #[serde(skip_serializing, default = "Default::default")]
    pub ai_api_key: String,
    #[serde(skip_serializing, default = "Default::default")]
    pub ai_api_key_encrypted: bool,
    #[serde(skip_serializing, default = "Default::default")]
    pub ai_model_name: String,
}

impl AppConfig {
    /// 获取配置文件路径
    pub fn config_path() -> PathBuf {
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
        
        // 创建默认AI配置
        let default_ai_config = AIConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "SiliconFlow".to_string(),
            api_url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
            api_key: "".to_string(),
            api_key_encrypted: false,
            model_name: "Qwen/Qwen3-8B".to_string(),
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
            // 默认使用直接搜索
            search_mode: "direct_search".to_string(),
            // 自动清理旧文章配置
            enable_auto_cleanup: false,
            article_retention_days: 60, // 默认保留60天
            max_articles_per_feed: 1000, // 默认每个订阅源保留1000篇文章
            // AI配置默认值
            ai_configs: vec![default_ai_config.clone()],
            current_ai_config_id: default_ai_config.id,
            // 兼容旧版本配置的字段
            ai_api_url: "".to_string(),
            ai_api_key: "".to_string(),
            ai_api_key_encrypted: false,
            ai_model_name: "".to_string(),
        }
    }
    
    /// 加载配置或使用默认值
    pub fn load_or_default() -> Self {
        let path = Self::config_path();

        if let Ok(mut file) = File::open(path) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() {
                let mut config: Self = match serde_json::from_str::<Self>(&contents) {
                    Ok(mut config) => {
                        // 检查并更新数据库路径，确保使用正确的文件名
                        if config.database_path.ends_with("feeds.db") || config.database_path.ends_with("feed.db") {
                            config.database_path = "./feed.duckdb".to_string();
                            // 保存更新后的配置
                            config.save().ok();
                        }
                        config
                    },
                    Err(e) => {
                        // 解析失败，记录错误日志
                        log::error!("配置文件解析失败: {}", e);
                        // 解析失败，尝试兼容旧版本配置
                        return Self::load_legacy_config(&contents);
                    }
                };
                
                // 处理旧版本配置转换（如果需要）
                config = config.migrate_legacy_config();
                // 保存迁移后的配置
                config.save().ok();
                return config;
            }
        }
        Self::default()
    }
    
    /// 加载旧版本配置
    fn load_legacy_config(contents: &str) -> Self {
        // 定义旧版本配置结构
        #[derive(Deserialize)]
        struct LegacyConfig {
            database_path: Option<String>,
            theme: Option<String>,
            auto_refresh_interval: Option<u32>,
            user_agent: Option<String>,
            font_size: Option<f32>,
            window_width: Option<u32>,
            window_height: Option<u32>,
            show_tray_icon: Option<bool>,
            enable_notifications: Option<bool>,
            max_notifications: Option<usize>,
            notification_timeout_ms: Option<u64>,
            timezone: Option<String>,
            show_console: Option<bool>,
            search_mode: Option<String>,
            enable_auto_cleanup: Option<bool>,
            article_retention_days: Option<u32>,
            max_articles_per_feed: Option<u32>,
            ai_api_url: Option<String>,
            ai_api_key: Option<String>,
            ai_model_name: Option<String>,
        }
        
        let default = Self::default();
        
        match serde_json::from_str::<LegacyConfig>(contents) {
            Ok(legacy) => {
                // 创建默认配置
                let mut config = Self::default();
                
                // 更新从旧配置读取的值
                if let Some(db_path) = legacy.database_path {
                    config.database_path = db_path;
                }
                if let Some(theme) = legacy.theme {
                    config.theme = theme;
                }
                if let Some(interval) = legacy.auto_refresh_interval {
                    config.auto_refresh_interval = interval;
                }
                if let Some(ua) = legacy.user_agent {
                    config.user_agent = ua;
                }
                if let Some(font) = legacy.font_size {
                    config.font_size = font;
                }
                if let Some(width) = legacy.window_width {
                    config.window_width = width;
                }
                if let Some(height) = legacy.window_height {
                    config.window_height = height;
                }
                if let Some(tray) = legacy.show_tray_icon {
                    config.show_tray_icon = tray;
                }
                if let Some(notify) = legacy.enable_notifications {
                    config.enable_notifications = notify;
                }
                if let Some(max) = legacy.max_notifications {
                    config.max_notifications = max;
                }
                if let Some(timeout) = legacy.notification_timeout_ms {
                    config.notification_timeout_ms = timeout;
                }
                if let Some(tz) = legacy.timezone {
                    config.timezone = tz;
                }
                if let Some(console) = legacy.show_console {
                    config.show_console = console;
                }
                if let Some(mode) = legacy.search_mode {
                    config.search_mode = mode;
                }
                if let Some(cleanup) = legacy.enable_auto_cleanup {
                    config.enable_auto_cleanup = cleanup;
                }
                if let Some(days) = legacy.article_retention_days {
                    config.article_retention_days = days;
                }
                if let Some(max) = legacy.max_articles_per_feed {
                    config.max_articles_per_feed = max;
                }
                
                // 添加旧版本AI配置
                if let (Some(api_url), Some(api_key), Some(model_name)) = (
                    legacy.ai_api_url,
                    legacy.ai_api_key,
                    legacy.ai_model_name
                ) {
                    config.add_ai_config(
                        "Legacy Config",
                        &api_url,
                        &api_key,
                        &model_name
                    );
                }
                
                config
            }
            Err(e) => {
                // 解析失败，记录错误日志
                log::error!("旧版本配置解析失败: {}", e);
                // 解析失败，返回默认配置
                default
            }
        }
    }
    
    /// 迁移旧版本配置
    fn migrate_legacy_config(mut self) -> Self {
        // 检查是否需要迁移
        if self.ai_configs.is_empty() {
            // 创建默认AI配置
            let default_config = AIConfig {
                id: uuid::Uuid::new_v4().to_string(),
                name: "Default Config".to_string(),
                api_url: self.ai_api_url.clone(),
                api_key: self.ai_api_key.clone(),
                api_key_encrypted: self.ai_api_key_encrypted,
                model_name: self.ai_model_name.clone(),
            };
            
            self.ai_configs.push(default_config.clone());
            self.current_ai_config_id = default_config.id;
        } else {
            // 检查current_ai_config_id是否指向有效的配置
            if !self.ai_configs.iter().any(|config| config.id == self.current_ai_config_id) {
                // 如果无效，更新为第一个配置的ID
                if let Some(first_config) = self.ai_configs.first() {
                    self.current_ai_config_id = first_config.id.clone();
                }
            }
        }
        self
    }
    
    /// 获取当前AI配置
    pub fn get_current_ai_config(&self) -> Option<&AIConfig> {
        self.ai_configs.iter().find(|config| config.id == self.current_ai_config_id)
    }
    
    /// 获取当前AI配置的解密API密钥
    pub fn get_current_decrypted_api_key(&self) -> Result<String, CryptoError> {
        if let Some(current_config) = self.get_current_ai_config() {
            if current_config.api_key.is_empty() {
                return Ok("".to_string());
            }
            
            // 创建加密管理器
            let crypto_manager = CryptoManager::new()?;
            
            // 检查API密钥是否已加密，即使标记为未加密，也检查格式
            let is_encrypted = current_config.api_key_encrypted || crypto_manager.is_encrypted(&current_config.api_key);
            
            if !is_encrypted {
                return Ok(current_config.api_key.clone());
            }
            
            return crypto_manager.decrypt(&current_config.api_key);
        }
        Ok("".to_string())
    }
    
    /// 添加AI配置
    pub fn add_ai_config(&mut self, name: &str, api_url: &str, api_key: &str, model_name: &str) {
        let new_config = AIConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            api_key_encrypted: false,
            model_name: model_name.to_string(),
        };
        
        self.ai_configs.push(new_config.clone());
        // 如果是第一个配置，设置为当前配置
        if self.ai_configs.len() == 1 {
            self.current_ai_config_id = new_config.id;
        }
    }
    
    /// 保存配置到文件
    pub fn save(&mut self) -> Result<(), std::io::Error> {
        let path = Self::config_path();
        
        // 创建一个临时配置，用于保存到文件
        let mut config_to_save = self.clone();
        
        // 加密所有API密钥
        for ai_config in &mut config_to_save.ai_configs {
            if !ai_config.api_key.is_empty() {
                // 创建加密管理器
                match CryptoManager::new() {
                    Ok(crypto_manager) => {
                        // 检查API密钥是否已加密，即使标记为未加密，也检查格式
                        let is_encrypted = ai_config.api_key_encrypted || crypto_manager.is_encrypted(&ai_config.api_key);
                        
                        if !is_encrypted {
                            // 加密API密钥
                            match crypto_manager.encrypt(&ai_config.api_key) {
                                Ok(encrypted_key) => {
                                    ai_config.api_key = encrypted_key;
                                    ai_config.api_key_encrypted = true;
                                },
                                Err(e) => {
                                    log::error!("保存配置时加密API密钥失败: {:?}", e);
                                    // 加密失败，继续保存，但标记为未加密
                                    ai_config.api_key_encrypted = false;
                                },
                            }
                        }
                    },
                    Err(e) => {
                        log::error!("创建加密管理器失败: {:?}", e);
                        // 创建加密管理器失败，继续保存，但标记为未加密
                        ai_config.api_key_encrypted = false;
                    },
                }
            }
        }
        
        let json = serde_json::to_string_pretty(&config_to_save)?;
        let mut file = File::create(path)?;
        file.write_all(json.as_bytes())?;
        
        Ok(())
    }
    
    /// 切换AI配置
    pub fn switch_ai_config(&mut self, config_id: &str) -> bool {
        if self.ai_configs.iter().any(|config| config.id == config_id) {
            self.current_ai_config_id = config_id.to_string();
            return true;
        }
        false
    }
    
    /// 编辑AI配置
    pub fn edit_ai_config(&mut self, config_id: &str, name: &str, api_url: &str, api_key: &str, model_name: &str) -> bool {
        if let Some(config) = self.ai_configs.iter_mut().find(|c| c.id == config_id) {
            config.name = name.to_string();
            config.api_url = api_url.to_string();
            config.api_key = api_key.to_string();
            // 用户输入的API密钥是明文的，所以将api_key_encrypted设置为false
            config.api_key_encrypted = false;
            config.model_name = model_name.to_string();
            return true;
        }
        false
    }
    
    /// 删除AI配置
    pub fn delete_ai_config(&mut self, config_id: &str) -> bool {
        let initial_len = self.ai_configs.len();
        self.ai_configs.retain(|config| config.id != config_id);
        let deleted = self.ai_configs.len() != initial_len;
        
        // 如果删除的是当前使用的配置，更新current_ai_config_id为第一个配置的ID
        if deleted && self.current_ai_config_id == config_id
            && let Some(first_config) = self.ai_configs.first() {
            self.current_ai_config_id = first_config.id.clone();
        }
        
        deleted
    }
    
    /// 获取AI配置列表
    #[allow(unused)]
    pub fn get_ai_configs(&self) -> &Vec<AIConfig> {
        &self.ai_configs
    }
    
    /// 导出AI配置为JSON字符串
    pub fn export_ai_configs(&self) -> Result<String, serde_json::Error> {
        // 导出时加密所有API密钥
        let mut export_configs = self.ai_configs.clone();
        for config in &mut export_configs {
            if !config.api_key.is_empty() && !config.api_key_encrypted {
                match CryptoManager::new() {
                    Ok(crypto_manager) => {
                        if let Ok(encrypted_key) = crypto_manager.encrypt(&config.api_key) {
                            config.api_key = encrypted_key;
                            config.api_key_encrypted = true;
                        }
                    },
                    Err(e) => {
                        log::error!("创建加密管理器失败: {:?}", e);
                    },
                }
            }
        }
        serde_json::to_string_pretty(&export_configs)
    }
    
    /// 导入AI配置
    pub fn import_ai_configs(&mut self, json_str: &str) -> Result<(), serde_json::Error> {
        let imported_configs: Vec<AIConfig> = serde_json::from_str(json_str)?;
        self.ai_configs.extend(imported_configs);
        Ok(())
    }
}