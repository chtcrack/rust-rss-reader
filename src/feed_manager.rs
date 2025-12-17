// RSS源管理模块

use crate::models::{Feed, FeedGroup};
use crate::notification::NotificationManager;
use crate::storage::StorageManager;
use crate::utils::{get_domain_from_url, is_valid_url};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// RSS源管理器
pub struct FeedManager {
    /// 存储管理器
    storage: Arc<RwLock<StorageManager>>,
    /// 通知管理器
    notification_manager: Option<Arc<Mutex<NotificationManager>>>,
}

impl FeedManager {
    /// 创建新的RSS源管理器
    pub fn new(storage: Arc<RwLock<StorageManager>>) -> Self {
        Self {
            storage,
            notification_manager: None,
        }
    }

    /// 设置通知管理器
    pub fn set_notification_manager(&mut self, manager: Arc<Mutex<NotificationManager>>) {
        self.notification_manager = Some(manager);
    }

    /// 添加新的RSS源
    #[allow(unused)]
    pub async fn add_feed(&self, title: &str, url: &str, group: &str) -> anyhow::Result<Feed> {
        // 验证URL
        if !is_valid_url(url) {
            return Err(anyhow::anyhow!("无效的URL地址"));
        }

        // 验证标题
        if title.trim().is_empty() {
            return Err(anyhow::anyhow!("标题不能为空"));
        }

        // 创建Feed对象
        let mut feed = Feed {
            id: 0, // 将由数据库自动生成
            title: title.trim().to_string(),
            url: url.trim().to_string(),
            group: group.trim().to_string(),
            group_id: None,
            last_updated: None,
            description: String::new(),
            language: String::new(),
            link: String::new(),
            favicon: None,
            auto_update: true,         // 默认开启自动更新
            enable_notification: true, // 默认启用通知
            ai_auto_translate: false,   // 默认关闭AI自动翻译
        };

        // 尝试从URL获取网站链接作为link
        if let Some(domain) = get_domain_from_url(url) {
            feed.link = format!("https://{}", domain);
        }

        // 保存到数据库
        let mut storage = self.storage.write().await;
        let feed_id = storage.add_feed(feed.clone()).await?;

        // 设置返回的Feed的ID
        feed.id = feed_id;

        Ok(feed)
    }

    /// 获取所有RSS源
    pub async fn get_all_feeds(&self) -> anyhow::Result<Vec<Feed>> {
        let storage = self.storage.read().await;
        storage.get_all_feeds().await
    }

    /// 获取分组列表
    #[allow(unused)]
    pub async fn get_all_groups(&self) -> anyhow::Result<Vec<FeedGroup>> {
        let storage = self.storage.read().await;
        // 直接从数据库获取所有分组，包含真实的ID
        storage.get_all_groups().await
    }

    /// 更新RSS源
    #[allow(unused)]
    pub async fn update_feed(&self, feed: &Feed) -> anyhow::Result<()> {
        // 验证URL
        if !is_valid_url(&feed.url) {
            return Err(anyhow::anyhow!("无效的URL地址"));
        }

        // 验证标题
        if feed.title.trim().is_empty() {
            return Err(anyhow::anyhow!("标题不能为空"));
        }

        let mut storage = self.storage.write().await;
        storage.update_feed(feed).await
    }

    /// 更新RSS源的分组
    #[allow(unused)]
    pub async fn update_feed_group(&self, feed_id: i64, group: &str) -> anyhow::Result<()> {
        let mut storage = self.storage.write().await;
        let mut feeds = storage.get_all_feeds().await?;

        if let Some(feed) = feeds.iter_mut().find(|f| f.id == feed_id) {
            feed.group = group.trim().to_string();
            storage.update_feed(feed).await?;
        } else {
            return Err(anyhow::anyhow!("找不到指定的订阅源"));
        }

        Ok(())
    }

    /// 删除RSS源
    #[allow(unused)]
    pub async fn delete_feed(&self, feed_id: i64) -> anyhow::Result<()> {
        let mut storage = self.storage.write().await;
        storage.delete_feed(feed_id).await
    }

    /// 批量删除RSS源
    #[allow(unused)]
    pub async fn delete_feeds(&self, feed_ids: &[i64]) -> anyhow::Result<()> {
        let mut storage = self.storage.write().await;

        for &feed_id in feed_ids {
            storage.delete_feed(feed_id).await?;
        }

        Ok(())
    }

    /// 按分组获取RSS源
    #[allow(unused)]
    pub async fn get_feeds_by_group(&self, group: &str) -> anyhow::Result<Vec<Feed>> {
        let storage = self.storage.read().await;
        let feeds = storage.get_all_feeds().await?;

        // 过滤指定分组的订阅源
        let result: Vec<Feed> = feeds
            .into_iter()
            .filter(|feed| feed.group == group)
            .collect();

        Ok(result)
    }

    /// 搜索RSS源
    #[allow(unused)]
    pub async fn search_feeds(&self, query: &str) -> anyhow::Result<Vec<Feed>> {
        let storage = self.storage.read().await;
        let feeds = storage.get_all_feeds().await?;

        let query_lower = query.to_lowercase();

        // 根据标题、URL或分组搜索
        let result: Vec<Feed> = feeds
            .into_iter()
            .filter(|feed| {
                feed.title.to_lowercase().contains(&query_lower)
                    || feed.url.to_lowercase().contains(&query_lower)
                    || feed.group.to_lowercase().contains(&query_lower)
            })
            .collect();

        Ok(result)
    }

    /// 检查RSS源是否已存在
    #[allow(unused)]
    pub async fn feed_exists(&self, url: &str) -> anyhow::Result<bool> {
        let storage = self.storage.read().await;
        let feeds = storage.get_all_feeds().await?;

        Ok(feeds.iter().any(|feed| feed.url == url))
    }

    /// 重命名分组
    pub async fn rename_group(&self, group_id: i64, new_name: &str) -> anyhow::Result<()> {
        let new_name_trimmed = new_name.trim();
        if new_name_trimmed.is_empty() {
            return Err(anyhow::anyhow!("分组名称不能为空"));
        }
        log::debug!("feed_manger获取分组ID: {}，对应新名称: '{}'", group_id, new_name);
        let mut storage = self.storage.write().await;

        // 调用StorageManager的update_group_name方法，同时更新feed_groups表和所有相关订阅源
        storage
            .update_group_name(group_id, new_name_trimmed)
            .await?;

        Ok(())
    }
    
    /// 通过名称获取分组ID
    #[allow(unused)]
    pub async fn get_group_id_by_name(&self, group_name: &str) -> anyhow::Result<Option<i64>> {
        let storage = self.storage.read().await;
        let groups = storage.get_all_groups().await?;
        
        Ok(groups.into_iter().find(|g| g.name == group_name).map(|g| g.id))
    }

    /// 删除分组（将所有属于该分组的订阅源移动到未分组）
    #[allow(unused)]
    pub async fn delete_group(&self, group_id: i64) -> anyhow::Result<()> {
        let mut storage = self.storage.write().await;
        
        // 将所有属于该分组的订阅源移动到未分组
        storage.delete_group(group_id).await?;
        
        Ok(())
    }

    /// 统计每个分组的订阅源数量
    #[allow(unused)]
    pub async fn get_group_stats(
        &self,
    ) -> anyhow::Result<std::collections::HashMap<String, usize>> {
        let storage = self.storage.read().await;
        let feeds = storage.get_all_feeds().await?;

        let mut stats = std::collections::HashMap::new();

        for feed in feeds {
            *stats.entry(feed.group.clone()).or_insert(0) += 1;
        }

        Ok(stats)
    }
}
