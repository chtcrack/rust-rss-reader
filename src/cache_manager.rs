use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use serde_json;
use log::{debug, error, info};

/// 缓存条目结构
#[derive(Serialize, Deserialize, Debug)]
pub struct CacheEntry {
    /// 爬取的文章内容
    pub content: String,
    /// 缓存创建时间（Unix时间戳）
    pub timestamp: u64,
    /// 缓存过期时间（Unix时间戳，可选）
    pub expires_at: Option<u64>,
}

/// 缓存管理器
pub struct CacheManager {
    /// 缓存数据存储
    cache_data: HashMap<String, CacheEntry>,
    /// 缓存文件路径
    cache_file_path: PathBuf,
    /// 缓存过期时间（秒），默认7天
    cache_ttl: u64,
}

impl CacheManager {
    /// 创建新的缓存管理器
    pub fn new(cache_dir: Option<&str>, cache_ttl: Option<u64>) -> Self {
        // 确定缓存文件路径
        let mut cache_file_path = if let Some(dir) = cache_dir {
            PathBuf::from(dir)
        } else {
            // 默认使用应用程序数据目录
            let app_data_dir = dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("RustRSSReader");
            // 确保目录存在
            if let Err(e) = fs::create_dir_all(&app_data_dir) {
                error!("创建缓存目录失败: {}", e);
            }
            app_data_dir
        };
        cache_file_path.push("article_cache.json");

        // 初始化缓存管理器
        let mut manager = Self {
            cache_data: HashMap::new(),
            cache_file_path,
            cache_ttl: cache_ttl.unwrap_or(7 * 24 * 60 * 60), // 默认7天
        };

        // 加载缓存
        manager.load_cache();
        // 清理过期缓存
        manager.clean_expired_cache();

        manager
    }

    /// 加载缓存从文件
    fn load_cache(&mut self) {
        if !self.cache_file_path.exists() {
            debug!("缓存文件不存在，创建空缓存");
            return;
        }

        match File::open(&self.cache_file_path) {
            Ok(mut file) => {
                let mut contents = String::new();
                if let Err(e) = file.read_to_string(&mut contents) {
                    error!("读取缓存文件失败: {}", e);
                    return;
                }

                match serde_json::from_str(&contents) {
                    Ok(cache) => {
                        self.cache_data = cache;
                        info!("成功加载缓存，共 {} 条记录", self.cache_data.len());
                    }
                    Err(e) => {
                        error!("解析缓存文件失败: {}", e);
                        // 尝试修复损坏的缓存文件
                        self.handle_corrupted_cache();
                    }
                }
            }
            Err(e) => {
                error!("打开缓存文件失败: {}", e);
            }
        }
    }

    /// 处理损坏的缓存文件
    fn handle_corrupted_cache(&mut self) {
        // 备份损坏的文件
        let backup_path = self.cache_file_path.with_extension("json.backup");
        if let Err(e) = fs::copy(&self.cache_file_path, &backup_path) {
            error!("备份损坏的缓存文件失败: {}", e);
        } else {
            info!("已备份损坏的缓存文件到 {:?}", backup_path);
        }

        // 创建空缓存
        self.cache_data.clear();
        self.save_cache().unwrap_or_else(|e| {
            error!("创建新缓存文件失败: {}", e);
        });
    }

    /// 保存缓存到文件
    pub fn save_cache(&self) -> io::Result<()> {
        // 确保父目录存在
        if let Some(parent) = self.cache_file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = File::create(&self.cache_file_path)?;
        serde_json::to_writer_pretty(file, &self.cache_data)?;
        debug!("缓存已保存，文件: {:?}", self.cache_file_path);
        Ok(())
    }

    /// 获取缓存项
    pub fn get(&mut self, key: &str) -> Option<String> {
        if let Some(entry) = self.cache_data.get(key) {
            // 检查是否过期
            let now = Self::current_timestamp();
            if entry.expires_at.is_some_and(|expires| expires < now) {
                // 过期了，删除缓存
                self.cache_data.remove(key);
                // 异步保存更新后的缓存
                if let Err(e) = self.save_cache() {
                    error!("保存缓存失败: {}", e);
                }
                return None;
            }
            // 返回缓存的内容
            return Some(entry.content.clone());
        }
        None
    }

    /// 设置缓存项
    pub fn set(&mut self, key: &str, content: String, custom_ttl: Option<u64>) -> io::Result<()> {
        let now = Self::current_timestamp();
        let ttl = custom_ttl.unwrap_or(self.cache_ttl);
        let expires_at = if ttl > 0 {
            Some(now + ttl)
        } else {
            None // 永不过期
        };

        // 更新缓存
        self.cache_data.insert(
            key.to_string(),
            CacheEntry {
                content,
                timestamp: now,
                expires_at,
            },
        );

        // 保存到文件
        self.save_cache()
    }

    /// 清理过期缓存
    pub fn clean_expired_cache(&mut self) {
        let now = Self::current_timestamp();
        let initial_size = self.cache_data.len();

        // 过滤掉过期的缓存项
        self.cache_data.retain(|_, entry| {
            entry.expires_at.map_or(true, |expires| expires >= now)
        });

        let removed_count = initial_size - self.cache_data.len();
        if removed_count > 0 {
            info!("清理了 {} 条过期缓存", removed_count);
            // 保存清理后的缓存
            if let Err(e) = self.save_cache() {
                error!("保存清理后的缓存失败: {}", e);
            }
        }
    }

    /// 清除所有缓存
    pub fn clear_all(&mut self) -> io::Result<()> {
        self.cache_data.clear();
        info!("已清除所有缓存");
        self.save_cache()
    }

    /// 获取当前缓存大小
    pub fn size(&self) -> usize {
        self.cache_data.len()
    }

    /// 获取当前Unix时间戳
    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0))
            .as_secs()
    }
}

// 实现Drop trait以确保程序退出时保存缓存
impl Drop for CacheManager {
    fn drop(&mut self) {
        if let Err(e) = self.save_cache() {
            error!("程序退出时保存缓存失败: {}", e);
        }
    }
}