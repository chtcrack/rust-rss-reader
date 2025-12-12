// 文章处理模块

use std::sync::Arc;
use std::sync::mpsc::Sender;
use tokio::sync::Mutex;

use crate::app::UiMessage;
use crate::models::Article;
use crate::notification::NotificationManager;
use crate::search::SearchManager;
use crate::storage::StorageManager;

/// 比较旧文章和新文章，返回新增的文章列表
pub fn get_new_articles(old_articles: &[Article], new_articles: &[Article]) -> Vec<Article> {
    // 创建旧文章GUID集合，用于快速查找
    // 优化：使用HashSet<&str>而不是HashSet<String>，避免克隆GUID字符串
    let old_guids: std::collections::HashSet<&str> = old_articles
        .iter()
        .map(|article| article.guid.as_str())
        .collect();
    
    // 预分配结果空间，假设新增文章数量为新文章的20%
    let mut results = Vec::with_capacity(new_articles.len() / 5);
    
    // 过滤出新增的文章
    for article in new_articles {
        if !old_guids.contains(article.guid.as_str()) {
            results.push(article.clone());
        }
    }
    
    results
}

/// 发送新文章通知的辅助函数
pub fn send_new_articles_notification(
    notification_manager: Option<Arc<Mutex<NotificationManager>>>,
    feed_title: &str,
    new_article_titles: &[String],
    enable_notification: bool,
) {
    // 检查是否有新文章、是否启用通知和通知管理器
    if new_article_titles.is_empty() || !enable_notification {
        return;
    }

    if let Some(notif_manager) = notification_manager.as_ref() {
        // 克隆必要的数据以在异步任务中使用
        let notif_manager_clone = notif_manager.clone();
        let feed_title_str = feed_title.to_string();
        let titles_clone = new_article_titles.to_vec();

        // 使用tokio::spawn异步发送通知，避免阻塞主流程
        tokio::spawn(async move {
            // 创建文章对象列表 - 优化：使用Vec::with_capacity预分配空间
            let mut feed_articles = Vec::with_capacity(titles_clone.len());
            
            for title in titles_clone.iter() {
                feed_articles.push((
                    feed_title_str.clone(), // 此处仍需克隆，因为每个元组需要独立所有权
                    Article {
                        id: 0,
                        feed_id: 0,
                        title: title.clone(),
                        link: "".to_string(),
                        author: "".to_string(),
                        pub_date: chrono::Utc::now(),
                        content: "".to_string(),
                        summary: "".to_string(),
                        is_read: false,
                        is_starred: false,
                        source: "".to_string(),
                        guid: "".to_string(),
                    },
                ));
            }

            notif_manager_clone
                .lock()
                .await
                .notify_new_articles(feed_articles);
        });
    } else {
        log::debug!("通知管理器不可用，跳过新文章通知");
    }
}

/// 直接搜索文章函数 - 从文章列表中搜索匹配的文章
pub fn direct_search_articles(query: &str, articles: &[Article], feed_id: i64) -> Vec<Article> {
    if query.trim().is_empty() {
        return Vec::new();
    }

    let query = query.trim().to_lowercase();
    let mut results = Vec::with_capacity(articles.len() / 2); // 预分配空间，假设平均匹配率为50%

    for article in articles {
        // 检查是否匹配订阅源ID
        if feed_id != -1 && article.feed_id != feed_id {
            continue;
        }

        // 优化：避免创建大字符串，分别检查各个字段
        // 优先检查短字段，提高匹配效率
        let title_lower = article.title.to_lowercase();
        if title_lower.contains(&query) {
            results.push(article.clone());
            continue;
        }
        
        let author_lower = article.author.to_lowercase();
        if author_lower.contains(&query) {
            results.push(article.clone());
            continue;
        }
        
        let summary_lower = article.summary.to_lowercase();
        if summary_lower.contains(&query) {
            results.push(article.clone());
            continue;
        }
        
        // 最后检查可能较大的content字段
        let content_lower = article.content.to_lowercase();
        if content_lower.contains(&query) {
            results.push(article.clone());
        }
    }

    // 按发布日期排序（最新的在前）
    results.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));

    results
}

/// 全局搜索文章函数 - 处理搜索请求并返回结果
pub async fn search_articles(
    query: String,
    _storage: Arc<Mutex<StorageManager>>,
    search_manager: Arc<Mutex<SearchManager>>,
    ui_tx: Sender<UiMessage>,
    feed_id: i64,
    search_mode: &str,
    articles: Option<&[Article]>,
) -> anyhow::Result<()> {
    log::info!(
        "开始搜索: {}, 订阅源ID: {}, 搜索方式: {}",
        query,
        feed_id,
        search_mode
    );

    let results = if search_mode == "direct_search" && articles.is_some() {
        // 使用直接搜索
        let start_time = std::time::Instant::now();
        let results = direct_search_articles(&query, articles.unwrap_or(&[]), feed_id);
        let duration = start_time.elapsed();
        log::info!(
            "直接搜索完成，找到 {} 篇文章，耗时 {:?}ms",
            results.len(),
            duration.as_millis()
        );
        results
    } else {
        // 使用索引搜索
        let start_time = std::time::Instant::now();
        let results = search_manager.lock().await.search(&query, feed_id).await;
        let duration = start_time.elapsed();
        log::info!(
            "索引搜索完成，找到 {} 篇文章，耗时 {:?}ms",
            results.len(),
            duration.as_millis()
        );
        results
    };

    // 发送搜索结果到UI
    if let Err(e) = ui_tx.send(UiMessage::SearchResults(results)) {
        log::error!("发送搜索结果失败: {}", e);
        return Err(anyhow::anyhow!("发送搜索结果失败: {}", e));
    }

    // 发送搜索完成消息
    if let Err(e) = ui_tx.send(UiMessage::SearchCompleted) {
        log::error!("发送搜索完成消息失败: {}", e);
    }

    Ok(())
}


