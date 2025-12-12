// 应用程序主模块

use eframe::egui;
use std::sync::Arc;
use tokio::sync::Mutex;
// use tokio::time;
// use regex::Regex;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;
// use image::DynamicImage;
use web_view::*;

// 导入自定义模块
use crate::ai_client::{AIChatSession, AIClient};
use crate::article_processor;
use crate::config::{AppConfig, convert_to_configured_timezone, APP_WINDOW_TITLE};
use crate::feed_manager::FeedManager;
use crate::models::{Article, Feed, FeedGroup};
use crate::notification::NotificationManager;
use crate::rss::RssFetcher;
use crate::search::SearchManager;
use crate::storage::StorageManager;
use crate::tray::{TrayManager, TrayMessage};

/// AI发送内容类型
#[derive(PartialEq)]
enum AISendContentType {
    /// 仅发送标题
    TitleOnly,
    /// 仅发送内容
    ContentOnly,
    /// 发送标题和内容
    Both,
}



/// 异步切换文章收藏状态的辅助函数
async fn toggle_article_starred_async(
    _storage: Arc<Mutex<StorageManager>>,
    article_id: i64,
    starred: bool,
    ui_tx: Sender<UiMessage>,
    current_is_read: bool,
) -> anyhow::Result<()> {
    // 更新数据库中的收藏状态
    let mut storage = _storage.lock().await;
    storage
        .update_article_starred_status(article_id, starred)
        .await?;

    // 发送UI更新消息，使用保存的已读状态
    ui_tx.send(UiMessage::ArticleStatusUpdated(
        article_id,
        current_is_read,
        starred,
    ))?;

    Ok(())
}

/// 执行自动更新的辅助函数
/// 内部更新订阅源函数，负责单个订阅源的更新逻辑
async fn update_feed_internal(
    feed_id: i64,
    feed_url: &str,
    _storage: Arc<Mutex<StorageManager>>,
    rss_fetcher: Arc<Mutex<RssFetcher>>,
    ai_client: Option<Arc<crate::ai_client::AIClient>>,
) -> anyhow::Result<()> {
    // 获取RSS内容
    let fetcher = rss_fetcher.lock().await;
    let mut articles = fetcher.fetch_feed(feed_url).await?;
    drop(fetcher); // 提前释放fetcher锁

    // 获取订阅源信息，检查是否启用AI自动翻译
    let storage_lock = _storage.lock().await;
    let feeds = storage_lock.get_all_feeds().await?;
    let feed = feeds.into_iter().find(|f| f.id == feed_id);
    drop(storage_lock); // 提前释放storage锁

    // 如果启用了AI自动翻译，并且有AI客户端，则进行翻译
    if let (Some(feed), Some(ai_client)) = (feed, ai_client)
        && feed.ai_auto_translate {
            log::info!("开始为订阅源 {} 翻译文章", feed.title);
            
            // 先检查哪些文章已经存在于数据库中，避免重复翻译
            let storage = _storage.lock().await;
            let existing_articles = storage.get_articles_by_feed(feed_id).await.unwrap_or_default();
            // 优化：使用&str作为HashSet的键，避免不必要的clone操作
            let existing_guids: std::collections::HashSet<&str> = existing_articles
                .iter()
                .map(|article| &article.guid[..])
                .collect();
            drop(storage); // 提前释放storage锁
            
            // 过滤出需要翻译的新文章
            let new_articles: Vec<_> = articles
                .into_iter()
                .filter(|article| !existing_guids.contains(&article.guid[..]))
                .collect();
            
            log::info!("发现 {} 篇新文章需要翻译", new_articles.len());
            
            // 使用独立线程执行翻译，避免阻塞主线程
            let translated_articles = tokio::spawn(async move {
                let mut translated_articles = Vec::new();
                
                for mut article in new_articles {
                    // 检测文章标题语言
                    let lang_result = ai_client.detect_language(&article.title).await;
                    
                    match lang_result {
                        Ok(lang) => {
                            log::info!("文章标题语言检测结果: {}", lang);
                            
                            // 如果不是中文，则进行翻译
                            if lang != "zh" {
                                log::info!("开始翻译文章: {}", article.title);
                                
                                // 翻译文章标题和内容
                                let translate_result = ai_client.translate_article(
                                    &article.title,
                                    &article.content,
                                ).await;
                                
                                match translate_result {
                                    Ok((translated_title, translated_content)) => {
                                        log::info!("文章翻译成功: {}", article.title);
                                        
                                        // 优化：直接修改article，避免不必要的clone操作
                                        // 因为new_articles是独立线程的局部变量，直接修改不会影响其他线程
                                        article.title = translated_title;
                                        article.content = translated_content;
                                        translated_articles.push(article);
                                    },
                                    Err(e) => {
                                        log::error!("文章翻译失败: {}, 错误: {:?}", article.title, e);
                                        // 翻译失败，使用原文
                                        translated_articles.push(article);
                                    }
                                }
                            } else {
                                // 中文文章，直接使用原文
                                translated_articles.push(article);
                            }
                        },
                        Err(e) => {
                            log::error!("语言检测失败: {}, 错误: {:?}", article.title, e);
                            // 语言检测失败，使用原文
                            translated_articles.push(article);
                        }
                    }
                }
                
                translated_articles
            }).await?;
            
            // 使用翻译后的文章
            articles = translated_articles;
        }

    // 更新订阅源信息和添加文章
    let mut storage_lock = _storage.lock().await;
    storage_lock.update_feed_last_updated(feed_id).await?;

    // 批量添加所有获取到的文章，数据库层面会处理重复文章的问题
    if !articles.is_empty() {
        storage_lock.add_articles(feed_id, articles).await?;
    }

    Ok(())
}

/// 从存储中加载特定订阅源的文章
pub async fn load_articles_for_feed(
    storage: Arc<Mutex<StorageManager>>,
    feed_id: i64,
) -> anyhow::Result<Vec<Article>> {
    let storage_lock = storage.lock().await;
    // 当选择全部文章时（feed_id = -1），使用get_all_articles而不是get_articles_by_feed
    if feed_id == -1 {
        storage_lock.get_all_articles().await
    } else {
        storage_lock.get_articles_by_feed(feed_id).await
    }
}

pub async fn perform_auto_update(
    _storage: Arc<Mutex<StorageManager>>,
    rss_fetcher: Arc<Mutex<RssFetcher>>,
    notification_manager: Option<Arc<Mutex<NotificationManager>>>,
    ui_tx: Sender<UiMessage>,
    search_manager: Option<Arc<Mutex<SearchManager>>>,
    ai_client: Option<Arc<crate::ai_client::AIClient>>,
) -> anyhow::Result<()> {
    log::info!("开始执行自动更新");

    // 获取所有订阅源
    let storage_lock = _storage.lock().await;
    let feeds = storage_lock.get_all_feeds().await?;
    drop(storage_lock); // 提前释放锁

    log::info!("发现 {} 个订阅源需要更新", feeds.len());

    let mut updated_count = 0;
    let mut error_count = 0;

    // 逐个更新订阅源并检查新文章
    for feed in feeds {
        // 检查是否需要自动更新
        if !feed.auto_update {
            log::info!(
                "跳过订阅源 (ID: {}, URL: {}), 自动更新已禁用",
                feed.id,
                feed.url
            );
            continue;
        }

        // 获取更新前的文章数量
        let storage_lock = _storage.lock().await;
        let old_articles = storage_lock.get_articles_by_feed(feed.id).await?;
        let old_article_count = old_articles.len();
        drop(storage_lock); // 提前释放锁

        // 更新订阅源
        if let Err(e) =
            update_feed_internal(feed.id, &feed.url, _storage.clone(), rss_fetcher.clone(), ai_client.clone()).await
        {
            log::error!(
                "更新订阅源失败 (ID: {}, URL: {}): {:?}",
                feed.id,
                feed.url,
                e
            );
            error_count += 1;
        } else {
            updated_count += 1;

            // 获取更新后的文章数量
            let storage_lock = _storage.lock().await;
            let new_articles = storage_lock.get_articles_by_feed(feed.id).await?;
            let current_article_count = new_articles.len();

            // 检查是否有新文章
            if current_article_count > old_article_count {
                // 获取真正的新文章
                let added_articles = article_processor::get_new_articles(&old_articles, &new_articles);
                let new_count = added_articles.len();
                
                // 初始化新文章标题向量
                let mut new_article_titles = Vec::new();
                
                if new_count > 0 {
                    log::info!("订阅源 {} 有 {} 篇新文章", feed.title, new_count);

                    // 按发布时间倒序排列，取最新的文章
                    let mut sorted_articles = added_articles; // 直接使用，无需clone
                    sorted_articles.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));

                    // 将所有新文章标题添加到通知列表
                    for article in sorted_articles.iter() {
                        new_article_titles.push(article.title.clone());
                    }
                }
                
                // 提前释放存储锁
                drop(storage_lock);

                // 如果有搜索管理器，异步更新搜索索引
                if let Some(search_mgr) = search_manager.as_ref() {
                    // 加载配置，判断是否需要更新搜索索引
                    let config = crate::config::AppConfig::load_or_default();
                    if config.search_mode == "index_search" {
                        // 克隆必要的数据以在异步任务中使用
                        let search_mgr_clone = search_mgr.clone();

                        // 筛选出新增的文章（按发布时间倒序，取最新的new_count篇）
                        let mut sorted_articles = new_articles; // 直接使用，无需clone
                        sorted_articles.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
                        let added_articles: Vec<Article> = 
                            sorted_articles.into_iter().take(new_count).collect();

                        // 使用tokio::spawn异步更新搜索索引，避免阻塞主流程
                            tokio::spawn(async move {
                                if !added_articles.is_empty() {
                                    log::info!(
                                        "正在更新搜索索引，添加 {} 篇新文章",
                                        added_articles.len()
                                    );
                                    let search_lock = search_mgr_clone.lock().await;
                                    // add_articles返回()而不是Result
                                    search_lock.add_articles(added_articles).await;
                                    log::info!("搜索索引更新完成");
                                }
                            });
                        }
                    }

                    // 使用封装的通知函数发送新文章通知，只在启用通知时发送
                    article_processor::send_new_articles_notification(
                        notification_manager.clone(),
                        &feed.title,
                        &new_article_titles,
                        feed.enable_notification,
                    );
                }
        }
    }

    log::info!(
        "自动更新完成: 更新成功 {} 个，失败 {} 个",
        updated_count,
        error_count
    );

    // 发送状态消息
    let status_msg = format!(
        "自动更新完成: 更新成功 {} 个，失败 {} 个",
        updated_count, error_count
    );
    let _ = ui_tx.send(UiMessage::StatusMessage(status_msg));

    // 发送全部更新完成消息
    let _ = ui_tx.send(UiMessage::AllFeedsRefreshed);

    Ok(())
}

/// 异步添加订阅源的辅助函数
#[allow(clippy::too_many_arguments)]
async fn add_feed_async(
    feed_url: String,
    feed_title: String,
    feed_group: String,
    auto_update: bool, // 添加自动更新参数
    ai_auto_translate: bool, // 添加AI自动翻译参数
    _storage: Arc<Mutex<StorageManager>>,
    rss_fetcher: Arc<Mutex<RssFetcher>>,
    _feed_manager: Arc<Mutex<FeedManager>>,
    ui_tx: Sender<UiMessage>, // 添加UI消息通道参数
) -> anyhow::Result<()> {
    if feed_url.is_empty() || feed_title.is_empty() {
        return Err(anyhow::anyhow!("URL和标题不能为空"));
    }

    // 验证URL格式
    let url = Url::parse(&feed_url)?;

    // 获取RSS内容
    let fetcher = rss_fetcher.lock().await;
    let articles = fetcher.fetch_feed(url.as_str()).await?;
    drop(fetcher); // 提前释放fetcher锁

    // 创建新的订阅源
    let new_feed = Feed {
        id: -1,                    // 将由数据库自动生成
        title: feed_title.clone(), // 使用clone，因为后面还需要使用feed_title
        url: feed_url,
        group: feed_group,
        group_id: None,
        description: String::new(), // 暂时为空，后续可能需要从其他地方获取
        last_updated: Some(chrono::Utc::now()),
        language: String::new(), // 暂时为空
        link: String::new(),     // 暂时为空
        favicon: None,
        auto_update,               // 使用传入的自动更新设置
        enable_notification: true, // 默认启用通知
        ai_auto_translate,         // 使用传入的AI自动翻译设置
    };

    // 保存订阅源到数据库
    let mut storage = _storage.lock().await;
    let feed_id = storage.add_feed(new_feed).await?;

    // 保存文章
    if !articles.is_empty() {
        let mut articles_to_save = articles;
        
        // 获取AI自动翻译设置，避免再次访问new_feed

        
        // 如果启用了AI自动翻译，对文章进行翻译
        if ai_auto_translate {
            log::info!("开始为新添加的订阅源 {} 翻译文章", feed_title);
            
            // 获取AI客户端
            let ai_client = AIClient::new(
                &crate::config::AppConfig::load_or_default().ai_api_url,
                &crate::config::AppConfig::load_or_default().ai_api_key,
                &crate::config::AppConfig::load_or_default().ai_model_name,
            );
            
            if let Ok(ai_client) = ai_client {
                // 使用独立线程执行翻译，避免阻塞主线程
                // 优化：使用clone，因为后面还需要使用articles_to_save
                let articles_to_translate = articles_to_save.clone();
                let translated_articles = tokio::spawn(async move {
                    let mut translated_articles = Vec::new();
                    
                    for article in articles_to_translate {
                        // 检测文章标题语言
                        let lang_result = ai_client.detect_language(&article.title).await;
                        
                        match lang_result {
                            Ok(lang) => {
                                log::info!("文章标题语言检测结果: {}", lang);
                                
                                // 如果不是中文，则进行翻译
                                if lang != "zh" {
                                    log::info!("开始翻译文章: {}", article.title);
                                    
                                    // 翻译文章标题和内容
                                    let translate_result = ai_client.translate_article(
                                        &article.title,
                                        &article.content,
                                    ).await;
                                    
                                    match translate_result {
                                        Ok((translated_title, translated_content)) => {
                                            log::info!("文章翻译成功: {}", article.title);
                                            
                                            // 直接修改文章，无需clone
                                            let mut translated_article = article;
                                            translated_article.title = translated_title;
                                            translated_article.content = translated_content;
                                            
                                            translated_articles.push(translated_article);
                                        },
                                        Err(e) => {
                                            log::error!("文章翻译失败: {}, 错误: {:?}", article.title, e);
                                            // 翻译失败，使用原文
                                            translated_articles.push(article);
                                        }
                                    }
                                } else {
                                    // 中文文章，直接使用原文
                                    translated_articles.push(article);
                                }
                            },
                            Err(e) => {
                                log::error!("语言检测失败: {}, 错误: {:?}", article.title, e);
                                // 语言检测失败，使用原文
                                translated_articles.push(article);
                            }
                        }
                    }
                    
                    translated_articles
                }).await;
                
                // 使用翻译后的文章
                if let Ok(translated) = translated_articles {
                    articles_to_save = translated;
                }
            }
        }
        
        let _article_count = articles_to_save.len();
        // 尝试批量保存文章
        if let Ok(new_articles_count) = storage.add_articles(feed_id, articles_to_save).await {
            log::info!("成功保存 {} 篇文章", new_articles_count);
        }
    }

    // 释放storage锁，避免在获取feeds时出现潜在的死锁
    drop(storage);

    // 重新加载所有订阅源并发送消息刷新UI
    log::debug!("添加订阅源后重新加载所有订阅源以更新UI");
    let feeds = _storage.lock().await.get_all_feeds().await?;

    // 发送FeedsLoaded消息来刷新UI
    log::debug!("发送FeedsLoaded消息，包含{}个订阅源", feeds.len());
    if let Err(e) = ui_tx.send(UiMessage::FeedsLoaded(feeds)) {
        log::error!("发送FeedsLoaded消息失败: {}", e);
    } else {
        // 添加SelectAllFeeds消息来确保UI完全刷新
        log::debug!("发送SelectAllFeeds消息来强制刷新UI");
        if let Err(e) = ui_tx.send(UiMessage::SelectAllFeeds) {
            log::error!("发送SelectAllFeeds消息失败: {}", e);
        }
    }

    // 发送状态消息
    let _ = ui_tx.send(UiMessage::StatusMessage(format!(
        "成功添加订阅源: {}",
        feed_title
    )));
    log::info!("成功添加订阅源: {}，已触发UI刷新", feed_title);

    Ok(())
}

/// 异步导入OPML文件的辅助函数
async fn import_opml_async(
    _storage: Arc<Mutex<StorageManager>>,
    _feed_manager: Arc<Mutex<FeedManager>>,
    file_path: &str,
) -> anyhow::Result<usize> {
    // 读取OPML文件内容
    let content = tokio::fs::read_to_string(file_path).await?;

    // 使用opml库解析OPML文件
    let opml = opml::OPML::from_str(&content)?;

    // 遍历解析出的大纲节点，使用迭代方式处理
    let mut feed_count = 0;
    let mut stack = Vec::new();
    
    // 初始化栈，包含根节点和当前分组
    for outline in &opml.body.outlines {
        stack.push((outline, String::new()));
    }
    
    while let Some((outline, current_group)) = stack.pop() {
        // 检查是否是RSS订阅源
        if outline.outlines.is_empty() && outline.r#type.as_deref() == Some("rss") {
            // 提取订阅源信息
            let title = if let Some(title) = outline.title.as_deref() {
                title.to_string()
            } else if !outline.text.is_empty() {
                outline.text.clone()
            } else {
                "未知标题".to_string()
            };
            let url = match outline.xml_url.as_deref() {
                Some(url) => url.trim().to_string(),
                None => continue, // 跳过没有XML URL的节点
            };

            // 检查URL是否已存在于系统中
            let url_exists = {
                let storage_lock = _storage.lock().await;
                let all_feeds = storage_lock.get_all_feeds().await?;
                all_feeds.iter().any(|f| f.url == url)
            };
            if url_exists {
                // URL已存在，跳过当前条目
                continue;
            }

            // 创建Feed对象
            let feed = Feed {
                id: 0, // 将由数据库自动生成
                title: title.trim().to_string(),
                url: url.clone(),
                group: current_group.trim().to_string(),
                group_id: None,
                last_updated: None,
                description: String::new(),
                language: String::new(),
                link: outline.html_url.as_deref().unwrap_or("").to_string(),
                favicon: None,
                auto_update: true,         // 默认开启自动更新
                enable_notification: true, // 默认启用通知
                ai_auto_translate: false,   // 默认关闭AI自动翻译
            };

            // 保存到数据库
            let mut storage = _storage.lock().await;
            storage.add_feed(feed).await?;
            feed_count += 1;
        } else {
            // 这是一个分组节点
            let group_name = if let Some(title) = outline.title.as_deref() {
                title.to_string()
            } else if !outline.text.is_empty() {
                outline.text.clone()
            } else {
                "未分组".to_string()
            };
            
            // 确保分组信息被添加到数据库中
            if !group_name.is_empty() {
                let mut storage = _storage.lock().await;
                // 添加分组，使用INSERT OR IGNORE避免重复
                storage.add_group(&group_name, None).await?;
            }
            
            // 将子节点压入栈中，注意顺序，确保按原顺序处理
            for child in outline.outlines.iter().rev() {
                stack.push((child, group_name.clone()));
            }
        }
    }

    Ok(feed_count)
}

/// 异步导出OPML文件的辅助函数
async fn export_opml_async(
    _storage: Arc<Mutex<StorageManager>>,
    file_path: &str,
) -> anyhow::Result<()> {
    // 获取所有订阅源
    let storage = _storage.lock().await;
    let feeds = storage.get_all_feeds().await?;

    // 生成符合OPML规范的XML内容
    let mut opml_content = String::new();
    opml_content.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?><opml version=\"1.0\">");
    opml_content.push_str("<head><title>RSS订阅源</title>");
    opml_content.push_str(&format!("<dateCreated>{}</dateCreated></head>", chrono::Utc::now().to_rfc2822()));
    opml_content.push_str("<body>");

    // 按分组组织订阅源
    let mut groups: std::collections::BTreeMap<String, Vec<&Feed>> = std::collections::BTreeMap::new();
    for feed in &feeds {
        let group_name = feed.group.clone();
        groups.entry(group_name).or_default().push(feed);
    }

    // 输出分组和订阅源
    for (group_name, feeds_in_group) in groups {
        if !group_name.is_empty() {
            opml_content.push_str(&format!(
                "<outline text=\"{}\" title=\"{}\">
",
                escape_xml(&group_name),
                escape_xml(&group_name)
            ));
        }

        for feed in feeds_in_group {
            opml_content.push_str(&format!(
                "<outline type=\"rss\" text=\"{}\" title=\"{}\" xmlUrl=\"{}\" htmlUrl=\"{}\" />
",
                escape_xml(&feed.title),
                escape_xml(&feed.title),
                escape_xml(&feed.url),
                escape_xml(&feed.link)
            ));
        }

        if !group_name.is_empty() {
            opml_content.push_str("</outline>
");
        }
    }

    opml_content.push_str("</body></opml>");

    // 写入文件
    tokio::fs::write(file_path, opml_content).await?;

    Ok(())
}

/// 简单的XML转义函数
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace("'", "&#39;")
}

// UI更新消息类型
#[derive(Debug)]
pub enum UiMessage {
    FeedsLoaded(Vec<Feed>),
    ArticlesLoaded(Vec<Article>),
    SearchArticles(String, i64),
    #[allow(unused)]
    FeedRefreshed(i64),
    AllFeedsRefreshed,
    ArticleStatusUpdated(i64, bool, bool), // article_id, is_read, is_starred
    StatusMessage(String),
    Error(String),
    UnreadCountUpdated(i32),
    GroupsLoaded(Vec<FeedGroup>),
    ClearSelectedFeed,
    SelectAllFeeds,                 // 选中全部订阅源
    UpdateIntervalLoaded(u64),      // 加载更新间隔配置
    AutoUpdateEnabledChanged(bool), // 自动更新状态变更
    ReloadArticles,                 // 重新加载文章列表
    SearchResults(Vec<Article>),    // 搜索结果
    SearchCompleted,                // 搜索完成
    RequestRepaint,                 // 请求UI重绘
    IndexingStarted(usize),         // 搜索索引开始更新，参数为总文章数
    AIStreamData(String),           // AI流式响应数据
    AIStreamEnd,                    // AI流式响应结束
    IndexingCompleted,              // 搜索索引更新完成
    #[allow(unused)]
    WebContentLoaded(String, String), // 网页内容加载完成，参数为内容和URL
    #[allow(unused)]
    SelectArticle(i64), // 选择并显示特定文章，参数为文章ID
    TranslationCompleted(String, String), // 文章翻译完成，参数为翻译后的标题和内容
}

// 用于解析HTML内容的枚举
#[derive(Debug, Clone)]
enum Part {
    Text(String),
    Heading(u32, String),
    Bold(String),
    Italic(String),
    Link(String, String),
    List(Vec<String>, bool), // 第二个参数表示是否为有序列表
    Paragraph(String),
    Quote(String),
    Code(String),
    #[allow(unused)]
    Table(Vec<Vec<String>>),
    Image(String),  // 图片URL
    LineBreak,      // 换行
    HorizontalRule, // 水平线
    #[allow(unused)]
    Div(Vec<Part>), // 容器标签
}

// 应用程序结构体
pub struct App {
    // 配置
    config: AppConfig,

    // 存储管理器
    storage: Arc<Mutex<StorageManager>>,

    // RSS获取器
    rss_fetcher: Arc<Mutex<RssFetcher>>,

    // 订阅源管理器
    feed_manager: Arc<Mutex<FeedManager>>,

    // 通知管理器
    notification_manager: Arc<Mutex<NotificationManager>>,

    // 搜索管理器
    search_manager: Arc<Mutex<SearchManager>>,

    // 搜索相关状态
    search_query: String,
    search_results: Vec<Article>,
    is_searching: bool,
    is_search_mode: bool,
    last_search_time: Option<Duration>,                // 搜索耗时
    search_input: String,                              // 用于输入防抖的字段
    search_debounce_timer: Option<std::time::Instant>, // 防抖定时器
    search_debounce_delay: std::time::Duration,        // 防抖延迟时间

    // UI状态
    selected_feed_id: Option<i64>,
    selected_article_id: Option<i64>,

    // 数据源和文章
    feeds: Vec<Feed>,
    articles: Vec<Article>,

    // 主题设置
    is_dark_mode: bool,
    font_size: f32,

    // 过滤和排序状态
    show_only_unread: bool,
    show_only_starred: bool,
    sort_by_date: bool,    // true: 按日期排序, false: 按标题排序
    sort_descending: bool, // true: 降序, false: 升序

    // 状态信息
    status_message: String,

    // 添加新源的状态
    new_feed_url: String,
    new_feed_title: String,
    new_feed_group: String,
    new_feed_auto_update: bool,
    new_feed_ai_auto_translate: bool,
    show_add_feed_dialog: bool,

    // 编辑订阅源状态
    edit_feed_id: Option<i64>,
    edit_feed_url: String,
    edit_feed_title: String,
    edit_feed_group: String,
    edit_feed_auto_update: bool,
    edit_feed_enable_notification: bool,
    edit_feed_ai_auto_translate: bool,
    show_edit_feed_dialog: bool,

    // 刷新状态
    is_refreshing: bool,

    // 初始化状态
    is_initializing: bool,

    // 窗口可见状态
    is_window_visible: bool,
    was_window_visible: bool, // 前一帧的窗口可见性状态，用于检测变化
    
    // 窗口句柄列表（在程序启动时获取）
    window_handles: Vec<*mut std::ffi::c_void>,

    // 自动更新相关
    last_refresh_time: u64,     // 上次刷新时间（Unix时间戳）
    auto_update_interval: u64,  // 自动更新间隔（分钟）
    auto_update_enabled: bool,  // 是否启用自动更新
    auto_update_countdown: u64, // 距离下次自动更新的剩余秒数
    auto_update_handle: Option<tokio::task::JoinHandle<()>>, // 自动更新任务句柄

    // 系统托盘管理
    tray_manager: Option<TrayManager>,

    // 系统托盘消息接收器
    tray_receiver: Option<Receiver<TrayMessage>>,
    
    // 托盘消息处理线程句柄
    tray_thread_handle: Option<std::thread::JoinHandle<()>>,

    // UI更新消息通道
    ui_tx: Sender<UiMessage>,
    ui_rx: Receiver<UiMessage>,

    // 导入导出状态
    is_importing: bool,
    is_exporting: bool,

    // 缓存的分组数据，避免每次都重新计算
    cached_groups: std::collections::BTreeMap<String, Vec<Feed>>,
    feeds_hash: Option<u64>,

    // 选中的文章ID集合
    selected_articles: std::collections::HashSet<i64>,

    // AI相关状态
    show_ai_send_dialog: bool,
    ai_send_content_type: AISendContentType,
    ai_send_preview: String,
    show_ai_chat_window: bool,
    show_ai_settings_dialog: bool,
    #[allow(unused)]
    ai_chat_messages: Vec<crate::ai_client::AIMessage>,
    ai_client: Option<AIClient>,
    ai_chat_session: Option<AIChatSession>,

    // AI流式相关状态
    is_ai_streaming: bool,
    current_ai_stream_response: String,
    is_sending_message: bool,

    // AI对话输入状态
    ai_chat_input: String,

    // AI设置窗口输入状态
    ai_settings_api_url: String,
    ai_settings_api_key: String,
    ai_settings_model_name: String,

    // 网页内容加载状态
    is_loading_web_content: bool,
    web_content: String,
    show_web_content: bool,
    current_article_url: String,

    // WebView相关
    #[allow(unused)]
    webview_handle: Option<web_view::Handle<()>>,

    // 分页相关
    page_size: usize,    // 每页显示的文章数量
    current_page: usize, // 当前页码

    // 加载状态
    is_indexing: bool,        // 搜索索引是否正在更新
    indexing_progress: usize, // 索引更新进度（可选）
    indexing_total: usize,    // 索引更新总数量（可选）

    // 编辑分组状态
    show_edit_group_dialog: bool,

    editing_group_name: String,
    new_group_name: String,

    // 翻译相关状态
    is_translating: bool,
    translated_article: Option<(String, String)>, // (translated_title, translated_content)
    show_translated_content: bool,
}

impl App {
    /// 创建新的应用实例
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::new_internal(cc, None, None)
    }

    /// 使用系统托盘创建新的应用实例
    pub fn new_with_tray(
        cc: &eframe::CreationContext<'_>,
        tray_manager: TrayManager,
        tray_receiver: Option<Receiver<TrayMessage>>,
    ) -> Self {
        Self::new_internal(cc, Some(tray_manager), tray_receiver)
    }

    /// 内部构造函数，用于共享初始化逻辑
    fn new_internal(
        cc: &eframe::CreationContext<'_>,
        tray_manager: Option<TrayManager>,
        tray_receiver: Option<Receiver<TrayMessage>>,
    ) -> Self {
        // 加载配置
        let config = AppConfig::load_or_default();

        // 初始化存储管理器
        let storage = Arc::new(Mutex::new(StorageManager::new(
            config.database_path.clone(),
        )));

        // 初始化RSS获取器
        let rss_fetcher = Arc::new(Mutex::new(RssFetcher::new(config.user_agent.clone())));

        // 初始化订阅源管理器
        let feed_manager = Arc::new(Mutex::new(FeedManager::new(storage.clone())));

        // 初始化通知管理器
        let notification_manager = Arc::new(Mutex::new(NotificationManager::new(config.clone())));

        // 初始化搜索管理器
        let search_manager = Arc::new(Mutex::new(SearchManager::new()));

        // 将通知管理器设置到订阅源管理器中
        {
            if let Ok(mut fm) = feed_manager.try_lock() {
                fm.set_notification_manager(notification_manager.clone());
            } else {
                log::warn!("无法初始化通知管理器，将在稍后尝试");
            }
        }

        // 检测系统主题（注意：当前eframe版本中IntegrationInfo没有system_theme字段）
        let is_dark_mode = config.theme == "dark"
            || (config.theme == "system" && cc.egui_ctx.style().visuals.dark_mode); // 使用egui上下文获取系统实际主题

        // 应用主题设置
        cc.egui_ctx.set_visuals(if is_dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });

        // 安装全局图像加载器，使UI能够直接识别和处理图像URL
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // 创建UI更新消息通道
        let (ui_tx, ui_rx) = channel();

        // 将UI消息发送器传递给通知管理器
        {
            if let Ok(mut notif_manager) = notification_manager.try_lock() {
                notif_manager.set_ui_sender(ui_tx.clone());
                log::debug!("已将UI消息发送器传递给通知管理器");
            } else {
                log::warn!("无法获取通知管理器锁，将在稍后尝试设置UI发送器");
            }
        }

        // 在新版本中，窗口大小设置需要使用不同的方式
        // 可以在App::update方法中使用ctx.send_viewport_cmd

        // 创建App实例
        let app = Self {
            config: config.clone(),
            storage: storage.clone(),
            rss_fetcher,
            feed_manager,
            notification_manager: notification_manager.clone(),
            selected_feed_id: Some(-1),
            selected_article_id: None,
            feeds: Vec::new(),
            articles: Vec::new(),
            search_manager: search_manager.clone(),
            search_query: String::new(),
            search_results: Vec::new(),
            is_searching: false,
            is_search_mode: false,
            last_search_time: None,
            search_input: String::new(),
            search_debounce_timer: None,
            search_debounce_delay: std::time::Duration::from_millis(300), // 300ms防抖延迟
            is_dark_mode,
            font_size: config.font_size,
            new_feed_url: String::new(),
            new_feed_title: String::new(),
            new_feed_group: String::new(),
            new_feed_auto_update: true, // 默认开启自动更新
            new_feed_ai_auto_translate: false, // 默认关闭AI自动翻译
            show_add_feed_dialog: false,
            edit_feed_id: None,
            edit_feed_url: String::new(),
            edit_feed_title: String::new(),
            edit_feed_group: String::new(),
            edit_feed_auto_update: true,         // 默认开启自动更新
            edit_feed_enable_notification: true, // 默认开启通知
            edit_feed_ai_auto_translate: false,  // 默认关闭AI自动翻译
            show_edit_feed_dialog: false,
            is_refreshing: false,
            is_initializing: false,
            show_only_unread: false,
            show_only_starred: false,
            sort_by_date: true,
            sort_descending: true,
            status_message: String::new(),
            last_refresh_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            auto_update_interval: 30,       // 默认30分钟
            auto_update_enabled: true,      // 默认启用
            auto_update_countdown: 30 * 60, // 默认30分钟，转换为秒
            auto_update_handle: None,
            tray_manager,
            tray_receiver,
            is_window_visible: true,
            was_window_visible: true, // 初始状态与is_window_visible一致,
            window_handles: Vec::new(),
            ui_tx: ui_tx.clone(),
            ui_rx,
            is_importing: false,
            is_exporting: false,
            cached_groups: std::collections::BTreeMap::new(),
            feeds_hash: None,

            // 选中的文章ID集合
            selected_articles: std::collections::HashSet::new(),

            // AI相关状态
            show_ai_send_dialog: false,
            ai_send_content_type: AISendContentType::Both,
            ai_send_preview: String::new(),
            show_ai_chat_window: false,
            show_ai_settings_dialog: false,
            ai_chat_messages: Vec::new(),
            ai_client: AIClient::new(
                &config.ai_api_url,
                &config.ai_api_key,
                &config.ai_model_name,
            )
            .ok(),
            ai_chat_session: None,

            // AI流式相关状态
            is_ai_streaming: false,
            current_ai_stream_response: String::new(),
            is_sending_message: false,

            // AI对话输入状态
            ai_chat_input: String::new(),

            // AI设置窗口输入状态
            ai_settings_api_url: config.ai_api_url.clone(),
            ai_settings_api_key: config.ai_api_key.clone(),
            ai_settings_model_name: config.ai_model_name.clone(),

            // 网页内容加载状态
            is_loading_web_content: false,
            web_content: String::new(),
            show_web_content: false,
            current_article_url: String::new(),

            // WebView相关
            webview_handle: None,

            page_size: 14,   // 默认每页显示50篇文章
            current_page: 1, // 当前页码从1开始

            // 初始化加载状态
            is_indexing: false,
            indexing_progress: 0,
            indexing_total: 0,

            // 初始化编辑分组状态
            show_edit_group_dialog: false,

            editing_group_name: String::new(),
            new_group_name: String::new(),

            // 初始化翻译相关状态
            is_translating: false,
            translated_article: None,
            show_translated_content: false,
            
            // 初始化托盘消息处理线程句柄
            tray_thread_handle: None,
        };

        // 调用异步初始化函数，传递search_manager
        Self::initialize_data_async(storage, ui_tx, search_manager.clone());

        // 启动托盘消息处理线程
        let mut app = app;
        app.start_tray_message_thread();

        app
    }

    /// 计算订阅源列表的哈希值，用于检测是否变化
    fn calculate_feeds_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        // 对每个订阅源的关键属性进行哈希
        for feed in &self.feeds {
            feed.id.hash(&mut hasher);
            feed.title.hash(&mut hasher);
            feed.url.hash(&mut hasher);
            feed.group.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// 更新AI发送内容预览
    fn update_ai_send_preview(&mut self) {
        let mut preview = String::new();

        // 获取选中的文章
        let selected_articles: Vec<&Article> = self
            .articles
            .iter()
            .filter(|article| self.selected_articles.contains(&article.id))
            .collect();

        for article in selected_articles.iter() {
            // preview.push_str(&format!("=== 文章 {} ===\n", index + 1));

            match self.ai_send_content_type {
                AISendContentType::TitleOnly => {
                    preview.push_str(&article.title);
                }
                AISendContentType::ContentOnly => {
                    preview.push_str(&article.content);
                }
                AISendContentType::Both => {
                    preview.push_str(&article.title);
                    preview.push_str("\n\n");
                    preview.push_str(&article.content);
                }
            }

            preview.push_str("\n\n");
        }

        self.ai_send_preview = preview;
    }

    /// 判断UI是否需要立即重绘
    /// 这个方法可以避免在后台更新时进行不必要的UI重绘
    fn is_visible_to_user(&self) -> bool {
        // 当窗口最小化到托盘时，不进行不必要的UI重绘
        self.is_window_visible
    }

    /// 生成并显示WebView窗口，用于加载完整网页内容
    fn spawn_webview(title: String, url: String) {
        // 在单独的线程中启动 webview
        std::thread::spawn(move || {
            web_view::builder()
                .title(&title)
                .content(Content::Url(&url))
                .size(1024, 768)
                .resizable(true)
                .debug(false)
                .user_data(())
                .invoke_handler(|webview, arg| {
                    if arg == "inject_script" {
                        // 动态注入脚本
                        let script = r#"
                        // 错误处理
                        window.console.error = function() {};
                        window.onerror = function() { return true; };
                        
                        // 自定义函数
                        window.customFunction = function() {
                            console.log('Custom function injected at runtime');
                        };
                        
                        customFunction(); // 立即执行
                    "#;
                        webview.eval(script)?;
                    }
                    Ok(())
                })
                .run()
                    .unwrap_or(());
        });
    }

    /// 启动托盘消息处理线程
    fn start_tray_message_thread(&mut self) {
        // 如果没有托盘接收器，直接返回
        let tray_receiver = self.tray_receiver.take();
        if tray_receiver.is_none() {
            return;
        }
        
        // 启动新线程处理托盘消息
        let thread_handle = std::thread::spawn(move || {
            log::info!("托盘消息处理线程已启动");
            
            let receiver = tray_receiver.unwrap_or_else(|| panic!("托盘接收器不可用，这是一个内部错误"));
            
            // 持续处理托盘消息
            loop {
                match receiver.recv() {
                    Ok(msg) => {
                        log::info!("收到托盘消息: {:?}", msg);
                        
                        match msg {
                            TrayMessage::ShowWindow => {
                                log::info!("处理显示窗口命令");
                                
                                // 使用 Windows API 显示窗口
                                unsafe {
                                    use windows_sys::Win32::UI::WindowsAndMessaging::{ SW_SHOW, ShowWindow, FindWindowW, EnumWindows};
                                    
                                    // 定义显示窗口的回调函数
                                    unsafe extern "system" fn enum_show_windows_callback(hwnd: *mut std::ffi::c_void, _lparam: isize) -> i32 {
                                        // 显示找到的窗口
                                        unsafe {
                                            ShowWindow(hwnd, SW_SHOW);
                                        }
                                        log::info!("枚举到窗口并显示，HWND: {:?}", hwnd);
                                        // 继续枚举所有窗口
                                        1
                                    }
                                    
                                    // 先尝试查找窗口
                                    let window_title = APP_WINDOW_TITLE;
                                    let mut title_utf16: Vec<u16> = window_title.encode_utf16().collect();
                                    title_utf16.push(0);
                                    
                                    let hwnd = FindWindowW(std::ptr::null(), title_utf16.as_ptr());
                                    if !hwnd.is_null() {
                                        log::info!("使用FindWindow找到窗口，HWND: {:?}", hwnd);
                                        ShowWindow(hwnd, SW_SHOW);
                                    } else {
                                        // 如果FindWindow失败，尝试枚举所有窗口
                                        log::warn!("使用FindWindow未找到窗口，尝试枚举所有窗口");
                                        let result = EnumWindows(
                                            Some(enum_show_windows_callback),
                                            0
                                        );
                                        log::info!("枚举窗口结果: {}", result);
                                    }
                                }
                            },
                            TrayMessage::HideWindow => {
                                log::info!("处理隐藏窗口命令");
                                
                                // 使用 Windows API 隐藏窗口
                                unsafe {
                                    use windows_sys::Win32::UI::WindowsAndMessaging::{ SW_HIDE, ShowWindow, FindWindowW, EnumWindows};
                                    
                                    // 定义隐藏窗口的回调函数
                                    unsafe extern "system" fn enum_hide_windows_callback(hwnd: *mut std::ffi::c_void, _lparam: isize) -> i32 {
                                        // 隐藏找到的窗口
                                        unsafe {
                                            ShowWindow(hwnd, SW_HIDE);
                                        }
                                        log::info!("枚举到窗口并隐藏，HWND: {:?}", hwnd);
                                        // 继续枚举所有窗口
                                        1
                                    }
                                    
                                    // 先尝试查找窗口
                                    let window_title = APP_WINDOW_TITLE;
                                    let mut title_utf16: Vec<u16> = window_title.encode_utf16().collect();
                                    title_utf16.push(0);
                                    
                                    let hwnd = FindWindowW(std::ptr::null(), title_utf16.as_ptr());
                                    if !hwnd.is_null() {
                                        log::info!("使用FindWindow找到窗口，HWND: {:?}", hwnd);
                                        ShowWindow(hwnd, SW_HIDE);
                                    } else {
                                        // 如果FindWindow失败，尝试枚举所有窗口
                                        log::warn!("使用FindWindow未找到窗口，尝试枚举所有窗口");
                                        let result = EnumWindows(
                                            Some(enum_hide_windows_callback),
                                            0
                                        );
                                        log::info!("枚举窗口结果: {}", result);
                                    }
                                }
                            },
                            TrayMessage::Exit => {
                                log::info!("处理退出命令");
                                // 退出程序
                                std::process::exit(0);
                            },
                        }
                    },
                    Err(e) => {
                        log::error!("接收托盘消息失败: {}", e);
                        break;
                    },
                }
            }
            
            log::info!("托盘消息处理线程已退出");
        });
        
        // 保存线程句柄
        self.tray_thread_handle = Some(thread_handle);
    }
    
    /// 检查并处理窗口可见性变化
    /// 当窗口从不可见变为可见时，重新加载文章数据以确保用户看到最新内容
    fn check_and_handle_visibility_change(&mut self, ctx: &egui::Context) {
        // 检查窗口可见性是否发生变化（从不可见变为可见）
        if !self.was_window_visible && self.is_window_visible {
            log::info!("检测到窗口可见性变化，从不可见变为可见，重新加载文章数据");

            // 如果有选中的订阅源，重新加载其文章
            if let Some(feed_id) = self.selected_feed_id {
                // 克隆storage的Arc引用，避免在非异步上下文中直接使用async方法
                let storage_clone = Arc::clone(&self.storage);
                let ui_tx_clone = self.ui_tx.clone();

                // 使用tokio::spawn在后台加载最新文章数据
                tokio::spawn(async move {
                    let storage_lock = storage_clone.lock().await;
                    let articles_result = if feed_id == -1 {
                        // 当选择全部文章时，获取所有文章
                        storage_lock.get_all_articles().await
                    } else {
                        // 否则获取指定订阅源的文章
                        storage_lock.get_articles_by_feed(feed_id).await
                    };

                    // 将加载的文章发送到UI线程进行显示
                    if let Ok(articles) = articles_result
                        && let Err(e) = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles)) {
                            log::error!("发送文章加载消息失败: {}", e);
                        }
                });
            }

            // 请求立即重绘UI
            ctx.request_repaint();
        }

        // 更新前一帧的窗口可见性状态
        self.was_window_visible = self.is_window_visible;
    }

    /// 启动自动更新后台任务
    pub fn start_auto_update(&mut self) {
        // 如果已有任务在运行，先停止
        self.stop_auto_update();

        if !self.auto_update_enabled {
            log::info!("自动更新已禁用，不启动定时任务");
            return;
        }

        log::info!("启动自动更新任务，间隔: {} 分钟", self.auto_update_interval);

        // 克隆必要的对象
        let storage_clone = self.storage.clone();
        let rss_fetcher_clone = self.rss_fetcher.clone();
        let ui_tx_clone = self.ui_tx.clone();
        let notification_manager_clone = self.notification_manager.clone();
        let search_manager_clone = self.search_manager.clone();
        let ai_client = self.ai_client.clone();
        let interval = self.auto_update_interval;

        // 启动后台任务
        let handle = tokio::spawn(async move {
            log::info!("自动更新任务已启动");

            loop {
                // 等待指定的间隔时间
                log::debug!("自动更新任务等待: {} 分钟", interval);
                tokio::time::sleep(tokio::time::Duration::from_secs(interval * 60)).await;

                        // 执行更新
                if let Err(e) = perform_auto_update(
                    storage_clone.clone(),
                    rss_fetcher_clone.clone(),
                    Some(notification_manager_clone.clone()),
                    ui_tx_clone.clone(),
                    Some(search_manager_clone.clone()),
                    ai_client.clone().map(Arc::new),
                )
                .await
                {
                    log::error!("自动更新失败: {:?}", e);
                    // 发送错误消息给UI
                    let _ = ui_tx_clone.send(UiMessage::Error(format!("自动更新失败: {:?}", e)));
                }
            }
        });

        // 保存任务句柄
        self.auto_update_handle = Some(handle);
    }

    /// 停止自动更新后台任务
    pub fn stop_auto_update(&mut self) {
        if let Some(handle) = self.auto_update_handle.take() {
            log::info!("停止自动更新任务");
            handle.abort();
        }
    }

    /// 初始化数据的异步函数（新方法）
    pub fn initialize_data_async(
        storage: Arc<Mutex<StorageManager>>,
        ui_tx: Sender<UiMessage>,
        search_manager: Arc<Mutex<SearchManager>>,
    ) {
        // 搜索管理器已通过参数传入
        log::debug!("开始异步初始化数据");
        tokio::spawn(async move {
            // 读取自动更新间隔配置
            {
                log::debug!("获取存储管理器的锁（读取更新间隔）");
                let storage_lock = storage.lock().await;
                if let Ok(update_interval) = storage_lock.get_update_interval().await {
                    log::info!("从数据库读取的自动更新间隔: {} 分钟", update_interval);
                    // 发送更新间隔消息给UI
                    let _ = ui_tx.send(UiMessage::UpdateIntervalLoaded(update_interval));
                } else {
                    log::error!("读取更新间隔配置失败，使用默认值");
                }
                // 离开代码块时自动释放锁
            }

            // 继续原有的初始化逻辑
            log::debug!("获取存储管理器的锁（加载数据）");
            let mut storage_lock = storage.lock().await;
            
            // 执行数据迁移，将group_name映射为group_id
            log::debug!("执行数据迁移，将group_name映射为group_id");
            if let Err(e) = storage_lock.migrate_feed_group_ids().await {
                log::debug!("数据迁移失败: {}", e);
            } else {
                log::info!("数据迁移成功完成");
                
                // 清理feeds表中的group_name字段
                log::debug!("清理feeds表中的group_name字段");
                if let Err(e) = storage_lock.cleanup_group_name_field().await {
                    log::error!("清理group_name字段失败: {}", e);
                } else {
                    log::info!("清理group_name字段成功");
                }
            }
            log::debug!("成功获取存储管理器的锁");

            // 加载所有订阅源
            log::debug!("开始加载所有订阅源");
            let feeds = match storage_lock.get_all_feeds().await {
                Ok(feeds_result) => {
                    log::info!("成功加载 {} 个订阅源", feeds_result.len());
                    log::debug!(
                        "订阅源列表: {:?}",
                        feeds_result.iter().map(|f| &f.title).collect::<Vec<_>>()
                    );
                    feeds_result
                }
                Err(e) => {
                    log::error!("加载订阅源失败: {}", e);
                    Vec::new()
                }
            };

            // 如果没有订阅源，添加一个示例订阅源
            let mut final_feeds = feeds;
            let mut final_articles = Vec::new();
            log::debug!(
                "检查是否需要添加示例订阅源，当前数量: {}",
                final_feeds.len()
            );
            if final_feeds.is_empty() {
                log::info!("没有找到订阅源，添加示例订阅源");

                // 创建一个示例订阅源
                log::debug!("创建示例订阅源");
                let example_feed = Feed {
                    auto_update: true,
                    ai_auto_translate: false,
                    id: 0, // 会在添加时自动生成
                    title: "欢迎使用 Rust RSS 阅读器".to_string(),
                    url: "example://welcome".to_string(),
                    group: "默认分组".to_string(),
                    group_id: None,
                    description: "这是一个示例订阅源，请点击'添加订阅源'按钮添加您自己的RSS源"
                        .to_string(),
                    last_updated: None,
                    language: "zh-CN".to_string(),
                    link: "#".to_string(),
                    favicon: None,
                    enable_notification: true, // 默认启用通知
                };

                // 克隆一份用于添加到数据库
                let feed_to_add = example_feed.clone();

                // 添加示例订阅源到数据库
                log::debug!("添加示例订阅源到数据库");
                match storage_lock.add_feed(feed_to_add).await {
                    Ok(feed_id) => {
                        log::debug!("示例订阅源添加成功，ID: {}", feed_id);
                        // 更新ID并添加到列表
                        let mut feed_with_id = example_feed;
                        feed_with_id.id = feed_id;
                        final_feeds.push(feed_with_id);
                        log::debug!("添加后订阅源数量: {}", final_feeds.len());

                        // 添加一个示例文章
                        log::debug!("添加示例文章");
                        let example_article = Article {
                            id: 0, // 会在添加时自动生成
                            feed_id,
                            title: "欢迎使用 Rust RSS 阅读器！".to_string(),
                            link: "#".to_string(),
                            author: "Rust RSS 团队".to_string(),
                            pub_date: chrono::Utc::now(),
                            content: "<h2>欢迎使用 Rust RSS 阅读器</h2><p>这是您的第一个订阅源和文章。</p><p>要添加新的RSS源，请点击左侧面板底部的'添加订阅源'按钮。</p><p>您可以添加任何符合RSS或Atom标准的订阅源URL。</p>".to_string(),
                            guid: format!("welcome-article-{}", chrono::Utc::now().timestamp()),
                            summary: "欢迎使用Rust RSS阅读器，点击了解如何添加RSS源。".to_string(),
                            is_read: false,
                            is_starred: false,
                            source: "示例源".to_string(),
                        };

                        // 使用add_articles方法（需要传递一个Vec）
                        log::debug!("将示例文章添加到数据库");
                        if let Err(e) = storage_lock
                            .add_articles(feed_id, vec![example_article])
                            .await
                        {
                            log::error!("添加示例文章失败: {}", e);
                        } else {
                            log::debug!("示例文章添加成功");
                        }
                    }
                    Err(e) => {
                        log::error!("添加示例订阅源失败: {}", e);
                        // 尝试直接创建内存中的订阅源用于UI显示
                        let mut memory_feed = example_feed;
                        memory_feed.id = -1; // 使用特殊ID表示内存中的订阅源
                        final_feeds.push(memory_feed);
                        log::debug!("添加内存中的订阅源作为备用，数量: {}", final_feeds.len());

                        // 为内存中的订阅源添加示例文章
                        log::debug!("为内存中的订阅源添加示例文章");
                        let example_article = Article {
                            id: -1, // 使用特殊ID表示内存中的文章
                            feed_id: -1,
                            title: "欢迎使用 Rust RSS 阅读器！".to_string(),
                            link: "#".to_string(),
                            author: "Rust RSS 团队".to_string(),
                            guid: "memory-welcome-article".to_string(),
                            pub_date: chrono::Utc::now(),
                            content: "<h2>欢迎使用 Rust RSS 阅读器</h2><p>这是您的第一个订阅源和文章。</p><p>要添加新的RSS源，请点击左侧面板底部的'添加订阅源'按钮。</p><p>您可以添加任何符合RSS或Atom标准的订阅源URL。</p>".to_string(),
                            summary: "欢迎使用Rust RSS阅读器，点击了解如何添加RSS源。".to_string(),
                            is_read: false,
                            is_starred: false,
                            source: "示例源".to_string(),
                        };
                        final_articles.push(example_article.clone());

                        // 添加到搜索索引
                        let config = crate::config::AppConfig::load_or_default();
                        if config.search_mode == "index_search" {
                            let search_lock = search_manager.lock().await;
                            search_lock.add_article(example_article).await;
                            log::debug!("示例文章已添加到搜索索引");
                        }

                        log::debug!("内存中的文章添加成功，数量: {}", final_articles.len());
                    }
                }
            }

            // 确保总是发送feeds消息，即使出错
            log::debug!("发送订阅源加载消息，数量: {}", final_feeds.len());
            if let Err(e) = ui_tx.send(UiMessage::FeedsLoaded(final_feeds.clone())) {
                log::error!("发送订阅源加载消息失败: {}", e);
            }

            // 如果有订阅源，尝试加载文章
            log::debug!("检查是否有订阅源，当前数量: {}", final_feeds.len());
            if !final_feeds.is_empty() {
                log::debug!("加载第一个订阅源(ID: {})的文章", final_feeds[0].id);

                // 检查是否是内存订阅源（ID为-1）
                if final_feeds[0].id == -1 {
                    log::debug!("使用内存订阅源，发送预创建的文章");
                    if final_articles.is_empty() {
                        // 如果final_articles为空，创建一个示例文章
                        let example_article = Article {
                            id: -1,
                            feed_id: -1,
                            title: "欢迎使用 Rust RSS 阅读器！".to_string(),
                            link: "#".to_string(),
                            guid: "memory-article-placeholder".to_string(),
                            author: "Rust RSS 团队".to_string(),
                            pub_date: chrono::Utc::now(),
                            content: "<h2>欢迎使用 Rust RSS 阅读器</h2><p>这是您的第一个订阅源和文章。</p><p>要添加新的RSS源，请点击左侧面板底部的'添加订阅源'按钮。</p><p>您可以添加任何符合RSS或Atom标准的订阅源URL。</p>".to_string(),
                            summary: "欢迎使用Rust RSS阅读器，点击了解如何添加RSS源。".to_string(),
                            is_read: false,
                            is_starred: false,
                            source: "示例源".to_string(),
                        };
                        final_articles.push(example_article.clone());

                        // 添加到搜索索引
                        let config = crate::config::AppConfig::load_or_default();
                        if config.search_mode == "index_search" {
                            let search_lock = search_manager.lock().await;
                            search_lock.add_article(example_article).await;
                            log::debug!("示例文章已添加到搜索索引");
                        }
                    }
                    log::debug!("发送内存文章，数量: {}", final_articles.len());
                    if let Err(e) = ui_tx.send(UiMessage::ArticlesLoaded(final_articles)) {
                        log::error!("发送内存文章失败: {}", e);
                    }
                } else {
                    // 正常从数据库加载当前选中订阅源的文章
                    match storage_lock.get_articles_by_feed(final_feeds[0].id).await {
                        Ok(articles) => {
                            log::debug!("成功加载 {} 篇文章", articles.len());

                            // 立即发送当前订阅源的文章给UI显示
                            log::debug!("发送文章加载消息");
                            if let Err(e) = ui_tx.send(UiMessage::ArticlesLoaded(articles)) {
                                log::error!("发送文章加载消息失败: {}", e);
                            }
                        }
                        Err(e) => {
                            log::error!("加载文章失败: {}", e);
                            // 发送空文章列表
                            if let Err(e) = ui_tx.send(UiMessage::ArticlesLoaded(Vec::new())) {
                                log::error!("发送空文章列表失败: {}", e);
                            }
                        }
                    }
                }
            } else {
                // 发送空文章列表
                if let Err(e) = ui_tx.send(UiMessage::ArticlesLoaded(Vec::new())) {
                    log::error!("发送空文章列表失败: {}", e);
                }

                // 通知用户数据库已初始化但没有订阅源
                if let Err(e) = ui_tx.send(UiMessage::StatusMessage(
                    "数据库已初始化，请添加您的第一个订阅源".to_string(),
                )) {
                    log::error!("发送状态消息失败: {}", e);
                }
            }

            // 在所有数据加载完成后，发送选中全部订阅源消息，确保默认显示所有文章
            log::debug!("所有文章加载完成，发送选中全部订阅源消息");
            if let Err(e) = ui_tx.send(UiMessage::SelectAllFeeds) {
                log::error!("发送SelectAllFeeds消息失败: {}", e);
            }

            // 尝试加载分组信息
            if let Ok(groups) = storage_lock.get_all_groups().await
                && let Err(e) = ui_tx.send(UiMessage::GroupsLoaded(groups)) {
                    log::error!("发送分组消息失败: {}", e);
                }

            // 尝试获取未读计数
            if let Ok(count) = storage_lock.get_unread_count().await
                && let Err(e) = ui_tx.send(UiMessage::UnreadCountUpdated(count as i32)) {
                    log::error!("发送未读计数失败: {}", e);
                }

            // 确保总是发送初始化完成消息
            if let Err(e) = ui_tx.send(UiMessage::StatusMessage("初始化完成".to_string())) {
                log::error!("发送状态消息失败: {}", e);
            }

            // 释放存储锁，准备启动后台索引任务
            drop(storage_lock);

            // 加载配置，判断是否需要构建搜索索引
            let config = crate::config::AppConfig::load_or_default();
            // 只有当搜索方式设置为index_search时，才在后台构建搜索索引
            if config.search_mode == "index_search" {
                log::debug!("启动后台搜索索引构建任务");
                let storage_clone = storage.clone();
                let ui_tx_clone = ui_tx.clone();
                let search_manager_clone = search_manager.clone();
                let final_feeds_clone = final_feeds.clone();

                tokio::spawn(async move {
                    log::debug!("后台索引任务开始执行");
                    
                    // 收集所有文章用于批量添加到搜索索引
                    let mut all_articles = Vec::new();
                    
                    // 先获取storage锁，收集所有文章
                    {
                        log::debug!("获取storage锁");
                        let storage_lock = storage_clone.lock().await;
                        log::debug!("成功获取storage锁");

                        // 加载所有订阅源的文章并收集
                        log::debug!("开始加载所有订阅源的文章用于构建搜索索引");
                        for feed in &final_feeds_clone {
                            if feed.id > 0 {
                                // 只处理有效的数据库订阅源
                                log::debug!("加载订阅源(ID: {})的文章用于索引", feed.id);
                                match storage_lock.get_articles_by_feed(feed.id).await {
                                    Ok(more_articles) => {
                                        log::debug!(
                                            "成功加载订阅源 {} 的 {} 篇文章用于索引",
                                            feed.title,
                                            more_articles.len()
                                        );
                                        // 将这些文章添加到总集合
                                        all_articles.extend(more_articles.clone());
                                    }
                                    Err(e) => {
                                        log::error!("加载订阅源 {} 的文章失败: {}", feed.title, e);
                                    }
                                }
                            }
                        }
                    } // storage_lock超出作用域，自动释放

                    let total_articles_added = all_articles.len();
                    log::info!("总共收集到 {} 篇文章用于构建搜索索引", total_articles_added);

                    // 发送索引开始消息
                    if let Err(e) =
                        ui_tx_clone.send(UiMessage::IndexingStarted(total_articles_added))
                    {
                        log::error!("发送索引开始消息失败: {}", e);
                    }

                    // 批量添加所有文章到搜索索引
                    log::info!("开始批量添加 {} 篇文章到搜索索引", total_articles_added);
                    log::debug!("获取search_manager锁");
                    let search_lock = search_manager_clone.lock().await;
                    log::debug!("成功获取search_manager锁");
                    search_lock.add_articles(all_articles).await;
                    log::info!("总共将 {} 篇文章添加到搜索索引", total_articles_added);

                    // 发送索引完成消息
                    if let Err(e) = ui_tx_clone.send(UiMessage::IndexingCompleted) {
                        log::error!("发送索引完成消息失败: {}", e);
                    }
                });
            } else {
                log::debug!("搜索方式设置为direct_search，跳过搜索索引构建");
            }
        });
    }

    /// 执行搜索（用于防抖）
    fn execute_search(&mut self) {
        // 始终更新搜索查询，确保与搜索输入保持一致
        self.search_query = self.search_input.clone();
        
        // 检查是否需要执行搜索
        if self.search_input.is_empty() {
            // 空查询时退出搜索模式并重新加载文章
            self.search_results.clear();
            self.is_searching = false;
            self.is_search_mode = false;
            self.last_search_time = None;
            self.search_debounce_timer = None;
            
            // 根据当前选中的订阅源加载相应的文章
            if let Some(feed_id) = self.selected_feed_id {
                // 克隆storage的Arc引用，避免在非异步上下文中直接使用async方法
                let storage_clone = Arc::clone(&self.storage);
                let ui_tx_clone = self.ui_tx.clone();
                
                // 使用tokio::spawn在后台加载最新文章数据
                tokio::spawn(async move {
                    let storage_lock = storage_clone.lock().await;
                    let articles_result = if feed_id == -1 {
                        // 当选择全部文章时，获取所有文章
                        storage_lock.get_all_articles().await
                    } else {
                        // 否则获取指定订阅源的文章
                        storage_lock.get_articles_by_feed(feed_id).await
                    };
                    
                    // 将加载的文章发送到UI线程进行显示
                    if let Ok(articles) = articles_result
                        && let Err(e) = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles)) {
                            log::error!("发送文章加载消息失败: {}", e);
                        }
                });
            }
            return;
        }

        // 设置搜索状态为true
        self.is_searching = true;

        // 发送搜索请求到UI消息通道
        // 当 selected_feed_id 为 None 时，使用 -1 表示搜索所有订阅源
        let feed_id = self.selected_feed_id.unwrap_or(-1);
        if let Err(e) = self.ui_tx.send(UiMessage::SearchArticles(
            self.search_query.clone(),
            feed_id,
        )) {
            log::error!("发送搜索请求失败: {}", e);
        }
    }

    /// 处理搜索防抖逻辑
    fn handle_search_debounce(&mut self, ctx: &egui::Context) {
        // 检查搜索输入是否发生变化
        let input_changed = self.search_input != self.search_query;
        
        // 如果输入发生变化，更新防抖定时器
        if input_changed {
            self.search_debounce_timer = Some(std::time::Instant::now());
        }
        
        // 检查是否已经过了防抖延迟时间
        if let Some(instant) = self.search_debounce_timer {
            if instant.elapsed() >= self.search_debounce_delay {
                // 执行搜索
                self.execute_search();
                self.search_debounce_timer = None;
            } else {
                // 请求在剩余延迟时间后重绘，以便再次检查
                ctx.request_repaint_after(self.search_debounce_delay - instant.elapsed());
            }
        }
    }

    /// 过滤和排序文章 - 性能优化版
    fn filter_and_sort_articles(&self, articles: &[Article]) -> Vec<Article> {
        // 优化: 先过滤，再克隆，减少不必要的克隆操作
        let filtered_articles: Vec<Article> = articles
            .iter()
            .filter(|article| {
                // 使用单个filter条件替代多个retain操作
                (!self.show_only_unread || !article.is_read)
                    && (!self.show_only_starred || article.is_starred)
            })
            .cloned()
            .collect();

        // 创建可变引用进行排序
        let mut sorted_articles = filtered_articles;

        // 应用排序 - 保持原有逻辑但提高可读性
        sorted_articles.sort_by(|a, b| {
            if self.sort_by_date {
                // 按日期排序 - 日期比较效率较高
                if self.sort_descending {
                    b.pub_date.cmp(&a.pub_date)
                } else {
                    a.pub_date.cmp(&b.pub_date)
                }
            } else {
                // 按标题排序 - 可以考虑添加不区分大小写的选项
                if self.sort_descending {
                    b.title.cmp(&a.title)
                } else {
                    a.title.cmp(&b.title)
                }
            }
        });

        sorted_articles
    }

    /// 渲染文章内容，使用缓存机制
    fn render_article_content_static(
        ui: &mut egui::Ui,
        content: &str,
        font_size: f32,
        base_url: &str,
    ) {
        // 直接解析HTML，不使用缓存（避免可变借用冲突）
        let mut text = content.to_string();

        // 预处理：移除脚本标签
        while let Some(start) = text.find("<script") {
            if let Some(end) = text[start..].find("</script>") {
                let end_pos = start + end + 9;
                // 确保end_pos不超出字符串长度，并且是一个有效的字符边界
                if end_pos <= text.len() && text.is_char_boundary(end_pos) {
                    text.replace_range(start..end_pos, "");
                } else {
                    // 如果超出边界，只删除到字符串末尾
                    text.replace_range(start.., "");
                    break;
                }
            } else {
                break;
            }
        }

        // 预处理：移除样式标签
        while let Some(start) = text.find("<style") {
            if let Some(end) = text[start..].find("</style>") {
                let end_pos = start + end + 8;
                // 确保end_pos不超出字符串长度，并且是一个有效的字符边界
                if end_pos <= text.len() && text.is_char_boundary(end_pos) {
                    text.replace_range(start..end_pos, "");
                } else {
                    // 如果超出边界，只删除到字符串末尾
                    text.replace_range(start.., "");
                    break;
                }
            } else {
                break;
            }
        }

        // 预处理：移除HTML注释
        while let Some(start) = text.find("<!--") {
            if let Some(end) = text[start..].find("-->") {
                let end_pos = start + end + 4;
                // 确保end_pos不超出字符串长度，并且是一个有效的字符边界
                if end_pos <= text.len() && text.is_char_boundary(end_pos) {
                    text.replace_range(start..end_pos, "");
                } else {
                    // 如果超出边界，只删除到字符串末尾
                    text.replace_range(start.., "");
                    break;
                }
            } else {
                break;
            }
        }

        // 预处理：移除不必要的属性（如style、class等），只保留必要的属性
        let mut processed_text = String::new();
        let mut in_tag = false;
        let mut tag_buffer = String::new();

        for c in text.chars() {
            if in_tag {
                tag_buffer.push(c);
                if c == '>' {
                    // 处理标签，移除不必要的属性
                    let processed_tag = Self::process_html_tag(&tag_buffer);
                    processed_text.push_str(&processed_tag);
                    tag_buffer.clear();
                    in_tag = false;
                }
            } else if c == '<' {
                in_tag = true;
                tag_buffer.push(c);
            } else {
                processed_text.push(c);
            }
        }

        // 使用处理后的文本
        text = processed_text;

        // 预处理：提取主要内容，移除无关内容
        text = Self::extract_main_content(&text);

        // 解析HTML并生成渲染部分
        let parts = Self::parse_html(&text, base_url);

        // 渲染处理
        Self::render_parts(ui, &parts, font_size);
    }

    /// 解析HTML文本，生成渲染所需的部分
    fn parse_html(text: &str, base_url: &str) -> Vec<Part> {
        let mut parts = Vec::new();
        let mut buffer = String::new();
        let mut in_tag = false;
        let mut tag_buffer = String::new();
        let mut in_list = false;
        let mut list_items: Vec<String> = Vec::new();
        let mut list_is_ordered = false;
        let mut in_quote = false;
        let mut quote_text = String::new();
        let mut in_code = false;
        let mut code_text = String::new();

        for c in text.chars() {
            if in_tag {
                tag_buffer.push(c);
                if c == '>' {
                    // 处理完整标签
                    Self::process_tag(
                        &tag_buffer,
                        &mut parts,
                        &mut buffer,
                        &mut in_list,
                        &mut list_items,
                        &mut list_is_ordered,
                        &mut in_quote,
                        &mut quote_text,
                        &mut in_code,
                        &mut code_text,
                        base_url,
                    );
                    tag_buffer.clear();
                    in_tag = false;
                }
            } else if c == '<' {
                // 保存缓冲区文本
                if !buffer.is_empty() {
                    if in_list {
                        // 列表项内的文本
                        if let Some(last_item) = list_items.last_mut() {
                            last_item.push_str(&buffer);
                        }
                    } else if in_quote {
                        quote_text.push_str(&buffer);
                    } else if in_code {
                        code_text.push_str(&buffer);
                    } else {
                        parts.push(Part::Text(buffer.clone()));
                    }
                    buffer.clear();
                }
                in_tag = true;
                tag_buffer.push(c);
            } else {
                buffer.push(c);
            }
        }

        // 处理剩余的文本
        if !buffer.is_empty() {
            parts.push(Part::Text(buffer));
        }

        // 关闭未完成的标签
        if in_list {
            parts.push(Part::List(list_items.clone(), list_is_ordered));
        }
        if in_quote {
            parts.push(Part::Quote(quote_text));
        }
        if in_code {
            parts.push(Part::Code(code_text));
        }

        parts
    }

    /// 处理HTML标签，更新解析状态和部分列表
    #[allow(clippy::too_many_arguments)]
    fn process_tag(
        tag: &str,
        parts: &mut Vec<Part>,
        buffer: &mut String,
        in_list: &mut bool,
        list_items: &mut Vec<String>,
        list_is_ordered: &mut bool,
        in_quote: &mut bool,
        quote_text: &mut String,
        in_code: &mut bool,
        code_text: &mut String,
        base_url: &str,
    ) {
        let tag_lower = tag.to_lowercase();

        // 处理结束标签
        if tag_lower.starts_with("</") {
            match &tag_lower[2..tag_lower.len() - 1] {
                "ul" | "ol" => {
                    if *in_list {
                        parts.push(Part::List(list_items.clone(), *list_is_ordered));
                        list_items.clear();
                        *in_list = false;
                    }
                }
                "blockquote" => {
                    if *in_quote {
                        parts.push(Part::Quote(quote_text.clone()));
                        quote_text.clear();
                        *in_quote = false;
                    }
                }
                "code" | "pre" => {
                    if *in_code {
                        parts.push(Part::Code(code_text.clone()));
                        code_text.clear();
                        *in_code = false;
                    }
                }
                "p" => {
                    if !buffer.is_empty() {
                        parts.push(Part::Paragraph(buffer.clone()));
                        buffer.clear();
                    }
                }
                _ => {}
            }
            return;
        }

        // 处理开始标签
        if tag_lower.starts_with("<h") {
            if let Some(level) = tag_lower.chars().nth(2).and_then(|c| c.to_digit(10))
                && (1..=6).contains(&level)
                    && let Some(content) = Self::extract_tag_content(tag) {
                        parts.push(Part::Heading(level, content));
                    }
        } else if tag_lower.starts_with("<b") || tag_lower.starts_with("<strong") {
            if let Some(content) = Self::extract_tag_content(tag) {
                parts.push(Part::Bold(content));
            }
        } else if tag_lower.starts_with("<i") || tag_lower.starts_with("<em") {
            if let Some(content) = Self::extract_tag_content(tag) {
                parts.push(Part::Italic(content));
            }
        } else if tag_lower.starts_with("<a ") {
            if let Some(link_text) = Self::extract_tag_content(tag)
                && let Some(url) = Self::extract_link_url(tag) {
                    // 将相对路径转换为绝对路径
                    let absolute_url = Self::resolve_url(url, base_url);
                    parts.push(Part::Link(link_text, absolute_url));
                }
        } else if tag_lower.starts_with("<ul") {
            *in_list = true;
            *list_is_ordered = false;
        } else if tag_lower.starts_with("<ol") {
            *in_list = true;
            *list_is_ordered = true;
        } else if tag_lower.starts_with("<li") {
            if *in_list {
                list_items.push(String::new());
            }
        } else if tag_lower.starts_with("<blockquote") {
            *in_quote = true;
        } else if tag_lower.starts_with("<pre") || tag_lower.starts_with("<code") {
            *in_code = true;
        } else if tag_lower.starts_with("<p") {
            // 处理段落开始
        } else if tag_lower.starts_with("<img") {
            // 处理图片标签
            if let Some(src) = Self::extract_image_src(tag) {
                // 将相对路径转换为绝对路径
                let absolute_src = Self::resolve_url(src, base_url);
                parts.push(Part::Image(absolute_src));
            }
        } else if tag_lower.starts_with("<br") {
            // 处理换行标签
            parts.push(Part::LineBreak);
        } else if tag_lower.starts_with("<hr") {
            // 处理水平线标签
            parts.push(Part::HorizontalRule);
        }
    }

    /// 渲染解析后的HTML部分
    fn render_parts(ui: &mut egui::Ui, parts: &[Part], font_size: f32) {
        for part in parts {
            match part {
                Part::Text(text) => {
                    if !text.trim().is_empty() {
                        // 使用RichText设置字体大小，避免修改全局样式
                        ui.label(egui::RichText::new(text).size(font_size));
                        ui.add_space(4.0);
                    }
                }
                Part::Heading(level, text) => {
                    // 设置不同级别的标题大小
                    let heading_size = match level {
                        1 => font_size * 1.8,
                        2 => font_size * 1.5,
                        3 => font_size * 1.3,
                        4 => font_size * 1.2,
                        5 => font_size * 1.1,
                        _ => font_size,
                    };

                    // 使用RichText设置标题样式和大小，避免修改全局样式
                    ui.label(egui::RichText::new(text).heading().size(heading_size));
                    ui.add_space(8.0);
                }
                Part::Bold(text) => {
                    // 使用RichText设置粗体和字体大小
                    ui.label(egui::RichText::new(text).strong().size(font_size));
                    ui.add_space(4.0);
                }
                Part::Italic(text) => {
                    // 使用RichText设置斜体和字体大小
                    ui.label(egui::RichText::new(text).italics().size(font_size));
                    ui.add_space(4.0);
                }
                Part::Link(text, url) => {
                    if ui.link(text).clicked()
                        && let Err(e) = open::that(url) {
                            log::error!("Failed to open link: {}", e);
                        }
                    ui.add_space(4.0);
                }
                Part::List(items, is_ordered) => {
                    ui.vertical(|ui| {
                        for (index, item) in items.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if *is_ordered {
                                    // 使用RichText设置字体大小
                                    ui.label(
                                        egui::RichText::new(format!("{}. ", index + 1))
                                            .size(font_size),
                                    );
                                } else {
                                    // 使用RichText设置字体大小
                                    ui.label(egui::RichText::new("• ").size(font_size));
                                }
                                // 使用RichText设置字体大小
                                ui.label(egui::RichText::new(item).size(font_size));
                            });
                            ui.add_space(4.0);
                        }
                    });
                    ui.add_space(8.0);
                }
                Part::Paragraph(text) => {
                    // 使用RichText设置字体大小
                    ui.label(egui::RichText::new(text).size(font_size));
                    ui.add_space(8.0);
                }
                Part::Quote(text) => {
                    ui.horizontal_wrapped(|ui| {
                        // 使用RichText设置字体大小
                        ui.label(egui::RichText::new("> ").size(font_size));
                        // 使用RichText设置斜体和字体大小
                        ui.label(egui::RichText::new(text).italics().size(font_size));
                    });
                    ui.add_space(8.0);
                }
                Part::Code(text) => {
                    // 使用等宽字体和灰色背景渲染代码块
                    // 直接使用TextEdit的默认样式，不修改全局样式
                    ui.add(
                        egui::TextEdit::multiline(&mut text.to_string())
                            .desired_width(f32::INFINITY)
                            .code_editor()
                            .interactive(false),
                    );
                    ui.add_space(8.0);
                }
                Part::Table(rows) => {
                    // 简单表格渲染
                    if !rows.is_empty() {
                        let num_cols = rows[0].len();

                        for row in rows {
                            if row.len() != num_cols {
                                continue; // 跳过格式不正确的行
                            }

                            ui.horizontal(|ui| {
                                for (i, cell) in row.iter().enumerate() {
                                    // 使用RichText设置字体大小
                                    ui.label(egui::RichText::new(cell).size(font_size));
                                    if i < num_cols - 1 {
                                        ui.separator();
                                    }
                                }
                            });
                        }
                        ui.add_space(8.0);
                    }
                }
                Part::Image(url) => {
                    // 渲染图片 - 使用egui_extras全局图像加载器直接显示URL图片
                    let max_width = ui.available_width() - 20.0; // 留出一些边距

                    ui.group(|ui| {
                        // 直接使用URL创建图像部件，设置最大宽度
                        let image_widget = egui::Image::new(url).max_width(max_width);

                        // 将图像包装在可点击的区域中
                        if ui.add(image_widget).clicked() {
                            // 点击图片时打开原始URL
                            if let Err(e) = open::that(url) {
                                log::error!("Failed to open image: {}", e);
                            }
                        }

                        // 添加图片URL作为提示文本，使用RichText设置字体大小
                        ui.label(
                            egui::RichText::new(format!("图片URL: {}", url))
                                .size(font_size * 0.8)
                                .weak(),
                        );
                    });

                    ui.add_space(8.0);
                }
                Part::LineBreak => {
                    ui.add_space(8.0);
                }
                Part::HorizontalRule => {
                    ui.separator();
                    ui.add_space(16.0);
                }
                Part::Div(parts) => {
                    // 递归渲染Div内容
                    Self::render_parts(ui, parts, font_size);
                }
            }
        }
    }

    /// 从HTML标签中提取图片的src属性
    fn extract_image_src(tag: &str) -> Option<String> {
        let pattern = regex::Regex::new(r#"src\s*=\s*"([^"]+)"#).ok()?;
        if let Some(caps) = pattern.captures(tag) {
            return Some(caps[1].to_string());
        }
        // 尝试匹配单引号
        let pattern = regex::Regex::new(r#"src\s*=\s*'([^']+)"#).ok()?;
        if let Some(caps) = pattern.captures(tag) {
            return Some(caps[1].to_string());
        }
        // 尝试匹配无引号的情况
        let pattern = regex::Regex::new(r#"src\s*=\s*([^\s>]+)"#).ok()?;
        if let Some(caps) = pattern.captures(tag) {
            return Some(caps[1].to_string());
        }
        None
    }

    /// 从HTML标签中提取内容
    fn extract_tag_content(tag: &str) -> Option<String> {
        // 确保标签是一个完整的标签对
        if !tag.starts_with('<') || !tag.ends_with('>') {
            return None;
        }

        // 找到开始标签的结束位置
        let start = tag.find('>')? + 1;

        // 找到结束标签的开始位置
        let end = tag.rfind("</")?;

        // 确保开始位置在结束位置之前
        if start >= end {
            return None;
        }

        Some(tag[start..end].to_string())
    }

    /// 从<a>标签中提取URL
    fn extract_link_url(tag: &str) -> Option<String> {
        if let Some(href_start) = tag.find("href=") {
            let href_start = href_start + 6; // "href=" 的长度

            let quote_char = if let Some(quote) = tag[href_start..].chars().next() {
                if quote == '"' || quote == '\'' {
                    quote
                } else {
                    ' '
                }
            } else {
                return None;
            };

            let content_start = if quote_char == '"' || quote_char == '\'' {
                href_start + 1
            } else {
                href_start
            };

            if let Some(end) = if quote_char == '"' || quote_char == '\'' {
                tag[content_start..].find(quote_char)
            } else {
                tag[content_start..].find(' ')
            } {
                return Some(tag[content_start..content_start + end].to_string());
            }
        }
        None
    }

    /// 将相对URL转换为绝对URL
    fn resolve_url(url: String, base_url: &str) -> String {
        // 如果已经是绝对URL，直接返回
        if url.starts_with("http://") || url.starts_with("https://") {
            return url;
        }

        // 如果是协议相对URL，添加协议
        if url.starts_with("//")
            && let Some(protocol_end) = base_url.find("://") {
                let protocol = &base_url[..protocol_end + 3];
                return format!("{}{}", protocol, url);
            }

        // 如果是根相对URL，只保留base_url的协议和域名部分
        if url.starts_with("/")
            && let Some(domain_end) = base_url.find("//") {
                if let Some(path_start) = base_url[domain_end + 2..].find("/") {
                    let domain = &base_url[..domain_end + 2 + path_start];
                    return format!("{}{}", domain, url);
                } else {
                    // 没有路径，直接添加URL
                    return format!("{}{}", base_url, url);
                }
            }

        // 如果是相对URL，保留base_url的目录部分
        if let Some(path_end) = base_url.rfind("/") {
            let base_path = &base_url[..path_end + 1];

            // 处理相对路径中的../和./
            let mut parts: Vec<&str> = base_path.split('/').filter(|&p| !p.is_empty()).collect();

            for part in url.split('/') {
                if part == ".." {
                    if !parts.is_empty() {
                        parts.pop();
                    }
                } else if part != "." && !part.is_empty() {
                    parts.push(part);
                }
            }

            // 重新构建URL
            let mut result = String::new();
            if base_url.starts_with("http://") {
                result.push_str("http://");
            } else if base_url.starts_with("https://") {
                result.push_str("https://");
            }

            result.push_str(&parts.join("/"));
            return result;
        }

        // 无法解析，返回原始URL
        url
    }

    /// 处理HTML标签，移除不必要的属性
    fn process_html_tag(tag: &str) -> String {
        let tag_lower = tag.to_lowercase();

        // 对于自闭合标签，直接返回
        if tag_lower.starts_with("<br")
            || tag_lower.starts_with("<hr")
            || tag_lower.starts_with("<img")
        {
            return tag.to_string();
        }

        // 对于其他标签，只保留必要的属性
        let mut processed_tag = String::new();
        let mut in_attribute = false;
        let mut attribute_name = String::new();
        let mut attribute_value = String::new();
        let mut in_quote = false;
        let mut quote_char = '"';

        for c in tag.chars() {
            if in_attribute {
                if c == '=' && !in_quote {
                    // 属性名结束，开始属性值
                    attribute_name = attribute_name.trim().to_lowercase();
                } else if (c == '"' || c == '\'') && !in_quote {
                    // 开始引号
                    in_quote = true;
                    quote_char = c;
                } else if c == quote_char && in_quote {
                    // 结束引号
                    in_quote = false;

                    // 只保留必要的属性
                    if attribute_name == "href"
                        || attribute_name == "src"
                        || attribute_name == "alt"
                    {
                        processed_tag.push_str(&format!(
                            " {}={}{}{}",
                            attribute_name, quote_char, attribute_value, quote_char
                        ));
                    }

                    attribute_name.clear();
                    attribute_value.clear();
                } else if !in_quote && (c == ' ' || c == '>') {
                    // 属性值结束（没有引号）
                    if !attribute_name.is_empty() {
                        // 只保留必要的属性
                        if attribute_name == "href"
                            || attribute_name == "src"
                            || attribute_name == "alt"
                        {
                            processed_tag
                                .push_str(&format!(" {}={}", attribute_name, attribute_value));
                        }

                        attribute_name.clear();
                        attribute_value.clear();
                    }

                    if c == '>' {
                        in_attribute = false;
                    }
                } else if in_quote {
                    // 属性值内容
                    attribute_value.push(c);
                } else {
                    // 属性名或属性值内容（没有引号）
                    if c != ' ' {
                        attribute_value.push(c);
                    }
                }
            } else {
                processed_tag.push(c);

                if c == '>' {
                    // 标签结束
                    break;
                } else if c == ' ' {
                    // 开始属性
                    in_attribute = true;
                }
            }
        }

        processed_tag
    }

    /// 提取HTML中的主要内容，移除无关内容
    fn extract_main_content(html: &str) -> String {
        // 尝试寻找主要内容标签
        let main_tags = [
            "<main",
            "<article",
            "<div class=\"main\"",
            "<div id=\"main\"",
            "<div class=\"content\"",
            "<div id=\"content\"",
            "<div class=\"article\"",
            "<div class=\"post\"",
            "<div class=\"blog\"",
        ];

        for tag in &main_tags {
            if let Some(start) = html.find(tag) {
                // 找到开始标签，寻找对应的结束标签
                let tag_name = if tag.starts_with("<main") {
                    "main"
                } else if tag.starts_with("<article") {
                    "article"
                } else {
                    "div"
                };

                let end_tag = format!("</{}>", tag_name); // 修复：移除了多余的引号

                // 寻找对应的结束标签，考虑嵌套情况
                let mut depth = 1; // 修复：从1开始，因为已经找到了一个开始标签
                let mut end_pos = None;
                let mut cursor = start;

                // 找到开始标签的结束位置
                if let Some(tag_end) = html[cursor..].find('>') {
                    cursor += tag_end + 1;
                } else {
                    continue; // 如果找不到完整的开始标签，跳过
                }

                while cursor < html.len() {
                    if let Some(lt_pos) = html[cursor..].find('<') {
                        let tag_start = cursor + lt_pos;
                        let remaining = &html[tag_start..];

                        if remaining.starts_with(&format!("<{}", tag_name))
                            || (tag_name == "div" && remaining.starts_with("<div"))
                        {
                            // 找到嵌套的开始标签
                            depth += 1;
                        } else if remaining.starts_with(&end_tag) {
                            // 找到结束标签
                            depth -= 1;
                            if depth == 0 {
                                end_pos = Some(tag_start + end_tag.len());
                                break;
                            }
                        }

                        // 移动到下一个字符继续搜索
                        cursor = tag_start + 1;
                    } else {
                        // 没有找到更多的标签，退出循环
                        break;
                    }
                }

                if let Some(end) = end_pos {
                    // 返回主要内容
                    return html[start..end].to_string();
                }
            }
        }

        // 如果没有找到主要内容标签，返回原始HTML
        html.to_string()
    }
}



impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        
        // 仅在首次运行时获取窗口句柄
        if self.window_handles.is_empty() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
                
                // 窗口标题
                let window_title = APP_WINDOW_TITLE;
                
                // 将Rust字符串转换为UTF-16编码
                let mut title_utf16: Vec<u16> = window_title.encode_utf16().collect();
                title_utf16.push(0); // 添加终止符
                
                // 使用FindWindow查找指定标题的窗口
                let hwnd = FindWindowW(std::ptr::null(), title_utf16.as_ptr());
                
                // 如果找到窗口句柄，保存到窗口句柄列表
                if !hwnd.is_null() {
                    log::info!("使用FindWindow找到窗口，HWND: {:?}", hwnd);
                    self.window_handles.push(hwnd);
                } else {
                    log::warn!("未找到标题为\"{}\"的窗口", window_title);
                }
            }
        }

        // 确保主题设置一致，解决启动时主题显示异常问题
        // 检查当前egui上下文的主题是否与应用配置一致
        let current_visuals = ctx.style().visuals.dark_mode;
        if current_visuals != self.is_dark_mode {
            // 如果不一致，重新应用主题设置
            ctx.set_visuals(if self.is_dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }

        // 检查并处理窗口可见性变化
        self.check_and_handle_visibility_change(ctx);

        // 托盘消息现在由独立线程处理

        // 添加调试信息显示
        egui::TopBottomPanel::bottom("debug_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "主题: {}",
                    if self.is_dark_mode {
                        "暗色"
                    } else {
                        "亮色"
                    }
                ));
                ui.separator();
                ui.label(format!("订阅源数量: {}", self.feeds.len()));
                ui.separator();
                ui.label(format!("文章数量: {}", self.articles.len()));
            });
        });

        // 计算距离下次自动更新的剩余秒数
        if self.auto_update_enabled {
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let next_update_time = self.last_refresh_time + self.auto_update_interval * 60;
            self.auto_update_countdown = next_update_time.saturating_sub(current_time);

            // 只在需要显示倒计时时才请求每秒刷新一次，降低CPU占用
            if self.auto_update_countdown > 0 {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
        } else {
            self.auto_update_countdown = 0;
        }

        // 处理UI消息通道中的消息
        //log::debug!("进入update方法，准备处理UI消息");
        while let Ok(msg) = self.ui_rx.try_recv() {
            match msg {
                UiMessage::FeedsLoaded(feeds) => {
                    log::debug!("收到订阅源加载消息，数量: {}", feeds.len());
                    self.feeds = feeds;
                    self.is_importing = false;
                    // 清除缓存，强制下一次渲染时重新计算分组
                    self.feeds_hash = None;
                    // 如果有订阅源但没有选中的，自动选择第一个
                    if !self.feeds.is_empty() && self.selected_feed_id.is_none() {
                        log::debug!("自动选择第一个订阅源: {}", self.feeds[0].title);
                        self.selected_feed_id = Some(self.feeds[0].id);
                    }
                    log::debug!("当前选中的订阅源ID: {:?}", self.selected_feed_id);
                    // 无论UI是否可见都请求重绘，确保新添加的订阅源立即显示
                    log::debug!("收到FeedsLoaded消息，请求UI重绘以显示更新后的订阅源列表");
                    ctx.request_repaint();
                }
                UiMessage::IndexingStarted(total) => {
                    log::debug!("收到索引开始消息，总共需要索引 {} 篇文章", total);
                    self.is_indexing = true;
                    self.indexing_total = total;
                    self.indexing_progress = 0;
                    ctx.request_repaint();
                }
                UiMessage::IndexingCompleted => {
                    log::debug!("收到索引完成消息");
                    self.is_indexing = false;
                    ctx.request_repaint();
                }
                UiMessage::ArticlesLoaded(articles) => {
                    // 收到文章后重置刷新状态
                    self.is_refreshing = false;
                    // 重置初始化状态
                    self.is_initializing = false;
                    // 重置翻译相关状态
                    self.is_translating = false;
                    self.translated_article = None;
                    self.show_translated_content = false;

                    // 记录日志
                    // log::debug!("收到文章加载消息，数量: {}", articles.len());

                    // 只有在非搜索模式下才更新文章列表
                    if !self.is_search_mode {
                        // 更新文章列表
                        self.articles = articles;
                    }

                    // 只在UI可见时请求重绘
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::AIStreamData(data) => {
                    // 处理AI流式响应数据
                    self.current_ai_stream_response.push_str(&data);
                    ctx.request_repaint();
                }
                UiMessage::AIStreamEnd => {
                    // 处理AI流式响应结束
                    self.is_ai_streaming = false;
                    self.is_sending_message = false;

                    // 将流式响应内容添加到聊天会话中
                    if !self.current_ai_stream_response.is_empty() {
                        if let Some(session) = self.ai_chat_session.as_mut() {
                            // 使用公共方法添加助手回复
                            session.add_assistant_message(&self.current_ai_stream_response);

                            // 保存聊天历史记录
                            if let Err(e) = session.save_chat_history() {
                                log::error!("保存聊天历史记录失败: {}", e);
                            }
                        }
                        // 清空临时存储的流式响应内容
                        self.current_ai_stream_response.clear();
                    }

                    ctx.request_repaint();
                }
                UiMessage::FeedRefreshed(feed_id) => {
                    log::debug!("收到订阅源刷新消息，ID: {}", feed_id);
                    // 如果刷新的是当前选中的订阅源，重新加载其文章
                    if Some(feed_id) == self.selected_feed_id {
                        // 克隆必要的对象
                        let storage_clone = Arc::clone(&self.storage);
                        let ui_tx_clone = self.ui_tx.clone();
                        let is_visible = self.is_visible_to_user();

                        // 在异步任务中执行，避免阻塞UI线程
                        tokio::spawn(async move {
                            match load_articles_for_feed(storage_clone, feed_id).await {
                                Ok(articles) => {
                                    // 发送ArticlesLoaded消息到UI线程
                                    let _ = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles));

                                    // 如果UI可见，请求重绘
                                    if is_visible {
                                        let _ = ui_tx_clone.send(UiMessage::RequestRepaint);
                                    }
                                }
                                Err(e) => {
                                    log::error!("加载文章失败: {:?}", e);
                                    // 发送错误消息
                                    let _ = ui_tx_clone
                                        .send(UiMessage::Error(format!("加载文章失败: {:?}", e)));
                                }
                            }
                        });
                    }
                }
                UiMessage::AllFeedsRefreshed => {
                    // 重置刷新标志
                    self.is_refreshing = false;

                    // 更新上次刷新时间
                    self.last_refresh_time = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());

                    // 重新加载所有订阅源，以便更新分组列表
                    let storage_clone = Arc::clone(&self.storage);
                    let ui_tx_clone = self.ui_tx.clone();

                    // 在异步任务中执行，避免阻塞UI线程
                    tokio::spawn(async move {
                        // 重新加载所有订阅源
                        match storage_clone.lock().await.get_all_feeds().await {
                            Ok(feeds) => {
                                // 发送FeedsLoaded消息到UI线程，更新订阅源列表和分组
                                let _ = ui_tx_clone.send(UiMessage::FeedsLoaded(feeds));
                            }
                            Err(e) => {
                                log::error!("重新加载订阅源失败: {:?}", e);
                                // 发送错误消息
                                let _ = ui_tx_clone
                                    .send(UiMessage::Error(format!("重新加载订阅源失败: {:?}", e)));
                            }
                        }
                    });

                    // 重新加载当前选中订阅源的文章
                    if let Some(feed_id) = self.selected_feed_id {
                        // 克隆必要的对象
                        let storage_clone = Arc::clone(&self.storage);
                        let ui_tx_clone = self.ui_tx.clone();
                        let is_visible = self.is_visible_to_user();

                        // 在异步任务中执行，避免阻塞UI线程
                        tokio::spawn(async move {
                            // 使用新的异步消息来通知UI线程文章已加载
                            match load_articles_for_feed(storage_clone, feed_id).await {
                                Ok(articles) => {
                                    // 发送ArticlesLoaded消息到UI线程
                                    let _ = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles));

                                    // 如果UI可见，请求重绘
                                    if is_visible {
                                        let _ = ui_tx_clone.send(UiMessage::RequestRepaint);
                                    }
                                }
                                Err(e) => {
                                    log::error!("加载文章失败: {:?}", e);
                                    // 发送错误消息
                                    let _ = ui_tx_clone
                                        .send(UiMessage::Error(format!("加载文章失败: {:?}", e)));
                                }
                            }
                        });
                    }
                }
                UiMessage::ArticleStatusUpdated(article_id, is_read, is_starred) => {
                    // 更新文章状态
                    if let Some(article) = self.articles.iter_mut().find(|a| a.id == article_id) {
                        article.is_read = is_read;
                        article.is_starred = is_starred;
                        // 只在UI可见时请求重绘
                        if self.is_visible_to_user() {
                            ctx.request_repaint();
                        }
                    }
                }
                UiMessage::StatusMessage(message) => {
                    self.status_message = message;
                    self.is_exporting = false;
                    // 只在UI可见时请求重绘
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::Error(error) => {
                    log::error!("UI Error: {}", error);
                    // 截断错误信息，只显示前150个字符，避免破坏UI布局
                    let truncated_error = if error.len() > 150 {
                        format!("{:.150}...", error)
                    } else {
                        error
                    };
                    self.status_message = format!("错误: {}", truncated_error);
                    self.is_importing = false;
                    self.is_exporting = false;
                    // 重置刷新标志
                    self.is_refreshing = false;
                    // 重置网页内容加载状态
                    self.is_loading_web_content = false;
                    self.show_web_content = false;
                    // 重置翻译状态
                    self.is_translating = false;
                    // 只在UI可见时请求重绘
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::UnreadCountUpdated(count) => {
                    // 暂时忽略未读计数更新
                    log::debug!("收到未读计数更新: {}", count);
                    // 只在UI可见时请求重绘
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::SelectAllFeeds => {
                    // 设置选中全部订阅源
                    log::debug!("收到选中全部订阅源消息");
                    // 使用特殊ID -1表示选择全部
                    self.selected_feed_id = Some(-1);
                    // 获取所有文章
                    let storage_clone = Arc::clone(&self.storage);
                    let ui_tx_clone = self.ui_tx.clone();
                    let is_visible = self.is_visible_to_user();

                    // 在异步任务中执行，避免阻塞UI线程
                    tokio::spawn(async move {
                        match load_articles_for_feed(storage_clone, -1).await {
                            Ok(articles) => {
                                // 发送ArticlesLoaded消息到UI线程
                                let _ = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles));

                                // 如果UI可见，请求重绘
                                if is_visible {
                                    let _ = ui_tx_clone.send(UiMessage::RequestRepaint);
                                }
                            }
                            Err(e) => {
                                log::error!("加载全部文章失败: {:?}", e);
                                // 发送错误消息
                                let _ = ui_tx_clone
                                    .send(UiMessage::Error(format!("加载全部文章失败: {:?}", e)));
                            }
                        }
                    });
                }
                UiMessage::GroupsLoaded(groups) => {
                    // 暂时忽略分组加载
                    log::debug!("收到分组加载更新，数量: {}", groups.len());
                }
                UiMessage::WebContentLoaded(content, url) => {
                    log::debug!("收到网页内容加载完成消息，URL: {}", url);
                    self.is_loading_web_content = false;
                    self.web_content = content;
                    self.show_web_content = true;
                    // 请求UI重绘
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::ClearSelectedFeed => {
                    // 清除选中的订阅源和文章状态
                    log::debug!("收到清除选中订阅源消息");
                    // 不再使用None，而是设置为选择全部
                    self.selected_feed_id = Some(-1);
                    self.selected_article_id = None;
                    // 获取所有文章
                    let storage_clone = Arc::clone(&self.storage);
                    let ui_tx_clone = self.ui_tx.clone();
                    let is_visible = self.is_visible_to_user();

                    // 在异步任务中执行，避免阻塞UI线程
                    tokio::spawn(async move {
                        match load_articles_for_feed(storage_clone, -1).await {
                            Ok(articles) => {
                                // 发送ArticlesLoaded消息到UI线程
                                let _ = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles));

                                // 如果UI可见，请求重绘
                                if is_visible {
                                    let _ = ui_tx_clone.send(UiMessage::RequestRepaint);
                                }
                            }
                            Err(e) => {
                                log::error!("加载全部文章失败: {:?}", e);
                                // 发送错误消息
                                let _ = ui_tx_clone
                                    .send(UiMessage::Error(format!("加载全部文章失败: {:?}", e)));
                            }
                        }
                    });
                }
                UiMessage::UpdateIntervalLoaded(interval) => {
                    // 更新自动更新间隔
                    log::debug!("收到更新间隔加载消息: {}分钟", interval);
                    self.auto_update_interval = interval;
                    // 更新自动更新任务的间隔
                    // 由于set_auto_update_interval是异步的，这里直接重启任务
                    if self.auto_update_enabled {
                        self.start_auto_update();
                    }
                }
                UiMessage::AutoUpdateEnabledChanged(enabled) => {
                    // 更新自动更新启用状态
                    log::debug!("收到自动更新状态变更消息: {}", enabled);
                    self.auto_update_enabled = enabled;
                    // 根据新状态启动或停止自动更新任务
                    if enabled {
                        self.start_auto_update();
                    } else {
                        self.stop_auto_update();
                    }
                }
                UiMessage::ReloadArticles => {
                    // 重新加载当前选中订阅源的文章
                    log::debug!("收到重新加载文章列表消息");
                    if let Some(feed_id) = self.selected_feed_id {
                        // 克隆storage的Arc引用，避免移动整个self
                        let storage_clone = Arc::clone(&self.storage);
                        if let Ok(articles) = futures::executor::block_on(async move {
                            let storage_lock = storage_clone.lock().await;
                            // 当选择全部文章时（feed_id = -1），使用get_all_articles而不是get_articles_by_feed
                            if feed_id == -1 {
                                storage_lock.get_all_articles().await
                            } else {
                                storage_lock.get_articles_by_feed(feed_id).await
                            }
                        }) {
                            self.articles = articles;
                            // 清除当前选中的文章，避免删除后引用不存在的文章
                            self.selected_article_id = None;
                            // 只在UI可见时请求重绘
                            if self.is_visible_to_user() {
                                ctx.request_repaint();
                            }
                        }
                    }
                }
                UiMessage::SearchArticles(query, feed_id) => {
                    // 处理搜索请求，启动搜索任务
                    log::debug!(
                        "收到搜索请求消息，搜索关键词: {}, 订阅源ID: {}",
                        query,
                        feed_id
                    );
                    // 启动异步搜索任务
                    let ui_tx = self.ui_tx.clone();
                    let storage = self.storage.clone();
                    let search_manager = self.search_manager.clone();
                    let search_mode = self.config.search_mode.clone();

                    // 克隆当前文章列表，用于直接搜索
                    let articles = self.articles.clone();

                    // 创建异步任务执行搜索
                    tokio::spawn(async move {
                        // 调用搜索方法
                        if let Err(e) = article_processor::search_articles(
                            query,
                            storage,
                            search_manager,
                            ui_tx,
                            feed_id,
                            &search_mode,
                            Some(&articles),
                        )
                        .await
                        {
                            log::error!("搜索失败: {}", e);
                        }
                    });
                }
                UiMessage::SearchResults(articles) => {
                    // 处理搜索结果
                    log::debug!("收到搜索结果消息，共 {} 篇文章", articles.len());

                    // 设置搜索模式为true，确保搜索状态同步
                    self.is_search_mode = true;

                    // 保存当前选中的文章ID
                    let current_selected_id = self.selected_article_id;

                    // 更新文章列表
                    self.articles = articles;

                    // 检查选中的文章是否仍在新的结果中，如果不在则清除选中状态
                    if let Some(selected_id) = current_selected_id
                        && !self.articles.iter().any(|a| a.id == selected_id) {
                            self.selected_article_id = None;
                        }
                        // 如果在，则保持选中状态，不做任何操作

                    // 请求UI重绘以显示搜索结果
                    ctx.request_repaint();
                }
                UiMessage::SearchCompleted => {
                    // 处理搜索完成消息
                    log::debug!("收到搜索完成消息");

                    // 设置搜索状态为false
                    self.is_searching = false;

                    // 请求UI重绘
                    ctx.request_repaint();
                }
                UiMessage::RequestRepaint => {
                    // 处理重绘请求
                    log::debug!("收到重绘请求");
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::SelectArticle(article_id) => {
                    // 处理选择文章消息
                    log::debug!("收到选择文章消息，文章ID: {}", article_id);

                    // 重置翻译相关状态
                    self.is_translating = false;
                    self.translated_article = None;
                    self.show_translated_content = false;

                    // 检查当前文章列表中是否存在该文章
                    let article_exists = self.articles.iter().any(|a| a.id == article_id);

                    if article_exists {
                        // 如果文章存在，直接设置为选中状态
                        log::debug!("文章存在于当前列表中，直接选中");
                        self.selected_article_id = Some(article_id);
                        // 标记为已读
                        if let Some(article) = self
                            .articles
                            .iter_mut()
                            .find(|a| a.id == article_id)
                            && !article.is_read {
                                article.is_read = true;
                                // 发送文章状态更新消息
                                let ui_tx = self.ui_tx.clone();
                                let article_id_clone = article_id;
                                tokio::spawn(async move {
                                    let _ = ui_tx.send(UiMessage::ArticleStatusUpdated(
                                        article_id_clone,
                                        true,
                                        false,
                                    ));
                                });
                            }
                    } else {
                        // 如果文章不存在于当前列表中，尝试重新加载所有文章
                        log::debug!("文章不存在于当前列表中，尝试重新加载所有文章");
                        let storage_clone = Arc::clone(&self.storage);
                        let ui_tx_clone = self.ui_tx.clone();
                        let article_id_clone = article_id;

                        tokio::spawn(async move {
                            // 加载所有文章
                            match load_articles_for_feed(storage_clone, -1).await {
                                Ok(articles) => {
                                    // 发送ArticlesLoaded消息到UI线程
                                    let _ = ui_tx_clone.send(UiMessage::ArticlesLoaded(articles));
                                    // 再次发送SelectArticle消息，以便在文章加载完成后选择
                                    let _ = ui_tx_clone
                                        .send(UiMessage::SelectArticle(article_id_clone));
                                }
                                Err(e) => {
                                    log::error!("加载全部文章失败: {:?}", e);
                                    // 发送错误消息
                                    let _ = ui_tx_clone.send(UiMessage::Error(format!(
                                        "加载全部文章失败: {:?}",
                                        e
                                    )));
                                }
                            }
                        });

                        // 立即返回，等待下一次SelectArticle消息
                        return;
                    }

                    // 请求UI重绘，以显示选中的文章内容
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
                UiMessage::TranslationCompleted(translated_title, translated_content) => {
                    // 处理翻译完成消息
                    log::debug!("收到翻译完成消息");
                    // 保存翻译结果
                    self.translated_article = Some((translated_title, translated_content));
                    // 结束翻译状态
                    self.is_translating = false;
                    // 默认显示翻译后的内容
                    self.show_translated_content = true;
                    // 请求UI重绘
                    if self.is_visible_to_user() {
                        ctx.request_repaint();
                    }
                }
            }
        }

        // 首次更新时初始化数据
        if self.feeds.is_empty() {
            // 现在使用异步任务初始化数据
            self.is_initializing = true;
            let ui_tx = self.ui_tx.clone();
            let storage = self.storage.clone();

            // 调用初始化函数，传入storage、消息发送器和search_manager
            App::initialize_data_async(storage, ui_tx, self.search_manager.clone());
        } else if self.articles.is_empty() && self.selected_feed_id.is_some() {
            // 如果有订阅源但没有文章数据，立即添加示例文章
            let example_article = Article {
                id: 1,
                feed_id: self.selected_feed_id.unwrap_or(-1),
                title: "测试文章标题 - 示例内容".to_string(),
                content: "<h2>这是测试文章内容</h2><p>这是一段测试内容，用于验证文章显示功能。</p><p>如果您能看到这段文字，说明文章内容渲染正常。</p>".to_string(),
                summary: "测试文章摘要内容".to_string(),
                link: "https://example.com/article1".to_string(),
                author: "测试作者".to_string(),
                pub_date: chrono::Utc::now(),
                guid: "test-article-example-1".to_string(),
                is_read: false,
                is_starred: false,
                source: "示例源".to_string(),
            };

            self.articles.push(example_article);
        }

        // 检查是否需要自动刷新
        if !self.is_refreshing && self.config.auto_refresh_interval > 0 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());

            // 如果距离上次刷新时间超过了配置的间隔（分钟转换为秒）
            if now - self.last_refresh_time >= (self.config.auto_refresh_interval as u64 * 60) {
                // 使用异步任务进行自动刷新
                self.is_refreshing = true;
                ctx.request_repaint();
                let storage = self.storage.clone();
                let feed_manager = self.feed_manager.clone();
                let rss_fetcher = self.rss_fetcher.clone();
                let ui_tx = self.ui_tx.clone();
                let now_clone = now;

                tokio::spawn(async move {
                    // 直接在闭包中实现刷新所有订阅源的逻辑
                    // 先获取所有订阅源，然后释放feed_manager锁
                    let feeds = {
                        log::debug!("获取feed_manager锁");
                        let feed_manager_lock = feed_manager.lock().await;
                        log::debug!("成功获取feed_manager锁");
                        match feed_manager_lock.get_all_feeds().await {
                            Ok(feeds) => feeds,
                            Err(e) => {
                                log::error!("Failed to auto refresh feeds: {}", e);
                                if let Err(e) = ui_tx.send(UiMessage::Error(e.to_string())) {
                                    log::error!("发送错误消息失败: {}", e);
                                }
                                return;
                            }
                        }
                    };

                    for feed in feeds {
                        // 获取订阅源内容
                        let articles = match rss_fetcher.lock().await.fetch_feed(&feed.url).await {
                            Ok(articles) => articles,
                            Err(e) => {
                                log::error!("Failed to fetch feed {}: {}", feed.title, e);
                                continue;
                            }
                        };
                        
                        // 存储文章
                        if let Err(e) = storage.lock().await.add_articles(feed.id, articles).await {
                            log::error!(
                                "Failed to parse and store articles for feed {}: {}",
                                feed.title,
                                e
                            );
                        }
                    }

                    // 发送刷新完成消息
                    if let Err(e) = ui_tx.send(UiMessage::AllFeedsRefreshed) {
                        log::error!("发送刷新完成消息失败: {}", e);
                    }
                });

                self.last_refresh_time = now_clone;
            }
        }

        // 暂时注释掉窗口大小保存功能，因为eframe API变更
        // 在新版本中，我们可以通过ctx.send_viewport_cmd来设置窗口大小
        // 但获取当前窗口大小需要使用不同的API

        // 处理窗口关闭事件需要在专门的方法中进行，而不是在update方法中
        // update方法每一帧都会被调用，不应该在这里无条件调用minimize_to_tray

        // 处理系统托盘消息
        // 注意：当前的TrayManager实现不支持从应用接收消息，
        // 只支持从托盘发送消息到应用
        // 系统托盘的点击和菜单项操作已在tray.rs中处理

        // 检查是否需要保存配置
        self.config.save().ok();

        // 设置窗口标题
        // 在新版本中，我们需要使用正确的API来设置窗口标题
        // 暂时注释掉，因为eframe API变更

        // 主菜单栏
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("导入OPML").clicked() {
                        // 使用rfd库打开文件对话框选择OPML文件
                        let file = rfd::FileDialog::new()
                            .add_filter("OPML Files", &["opml", "xml"])
                            .pick_file();
                        
                        if let Some(file_path) = file {
                            self.is_importing = true;
                            ctx.request_repaint();
                            
                            // 克隆必要的参数而不是整个App实例
                            let storage = self.storage.clone();
                            let feed_manager = self.feed_manager.clone();
                            let ui_tx = self.ui_tx.clone();
                            
                            tokio::spawn(async move {
                                // 导入OPML文件的逻辑
                                match import_opml_async(storage.clone(), feed_manager, &file_path.display().to_string()).await {
                                    Ok(feed_count) => {
                                        log::info!("OPML导入成功，共{}个订阅源", feed_count);
                                        // 重新加载订阅源列表
                                        if let Ok(feeds) = storage.lock().await.get_all_feeds().await
                                            && let Err(e) = ui_tx.send(UiMessage::FeedsLoaded(feeds)) {
                                                log::error!("发送订阅源更新消息失败: {}", e);
                                            }
                                        if let Err(e) = ui_tx.send(UiMessage::StatusMessage(format!("OPML导入成功，共{}个订阅源", feed_count))) {
                                            log::error!("发送状态消息失败: {}", e);
                                        }
                                    },
                                    Err(e) => {
                                        log::error!("OPML导入失败: {}", e);
                                        if let Err(e) = ui_tx.send(UiMessage::Error(e.to_string())) {
                                            log::error!("发送错误消息失败: {}", e);
                                        }
                                    }
                                }
                            });
                        }
                        ui.close();
                    }
                    if ui.button("导出OPML").clicked() {
                        // 使用rfd库打开保存文件对话框
                        let file = rfd::FileDialog::new()
                            .add_filter("OPML Files", &["opml"])
                            .set_file_name("feeds.opml")
                            .save_file();
                        
                        if let Some(file_path) = file {
                            self.is_exporting = true;
                            ctx.request_repaint();
                            
                            // 克隆必要的参数而不是整个App实例
                            let storage = self.storage.clone();
                            let ui_tx = self.ui_tx.clone();
                            
                            tokio::spawn(async move {
                                // 导出OPML文件的逻辑
                                match export_opml_async(storage, &file_path.display().to_string()).await {
                                    Ok(_) => {
                                        log::info!("OPML导出成功到: {}", file_path.display());
                                        if let Err(e) = ui_tx.send(UiMessage::StatusMessage("OPML导出成功".to_string())) {
                                            log::error!("发送状态消息失败: {}", e);
                                        }
                                    },
                                    Err(e) => {
                                        log::error!("OPML导出失败: {}", e);
                                        if let Err(e) = ui_tx.send(UiMessage::Error(e.to_string())) {
                                            log::error!("发送错误消息失败: {}", e);
                                        }
                                    }
                                }
                            });
                        }
                        ui.close();
                    }
                    if ui.button("退出").clicked() {
                        // 直接退出程序
                        std::process::exit(0);
                    }
                });
                
                ui.menu_button("订阅源", |ui| {
                    if ui.button("添加订阅源").clicked() {
                        self.show_add_feed_dialog = true;
                        ui.close();
                    }
                    if ui.button("刷新所有").clicked() && !self.is_refreshing {
                        // 设置刷新标志
                        self.is_refreshing = true;
                        
                        // 克隆必要的参数而不是整个App实例
                        let storage = self.storage.clone();
                        let feed_manager = self.feed_manager.clone();
                        let rss_fetcher = self.rss_fetcher.clone();
                        let notification_manager = self.notification_manager.clone();
                        let ui_tx = self.ui_tx.clone();
                        tokio::spawn(async move {
                            // 直接在闭包中实现刷新所有订阅源的逻辑
                            // 先获取所有订阅源，释放feed_manager锁
                            let feeds = {
                                let feed_manager_lock = feed_manager.lock().await;
                                match feed_manager_lock.get_all_feeds().await {
                                    Ok(feeds) => feeds,
                                    Err(e) => {
                                        log::error!("Failed to get feeds: {}", e);
                                        return;
                                    }
                                }
                            }; // feed_manager_lock超出作用域，自动释放
                                
                            for feed in feeds {
                                // 获取更新前的文章
                                let old_articles = {
                                    let storage_lock = storage.lock().await;
                                    storage_lock.get_articles_by_feed(feed.id).await.unwrap_or_default()
                                };
                                let old_article_count = old_articles.len();
                                
                                // 获取订阅源内容
                                match rss_fetcher.lock().await.fetch_feed(&feed.url).await {
                                    Ok(articles) => {
                                        // 存储文章
                                        if let Err(e) = storage.lock().await.add_articles(feed.id, articles).await {
                                            log::error!("Failed to parse and store articles for feed {}: {}", feed.title, e);
                                        } else {
                                            // 获取更新后的文章数量
                                            let new_articles = storage.lock().await.get_articles_by_feed(feed.id).await.unwrap_or_default();
                                            let current_article_count = new_articles.len();
                                            
                                            // 检查是否有新文章
                                            if current_article_count > old_article_count {
                                                // 获取真正的新文章
                                                    let added_articles = article_processor::get_new_articles(&old_articles, &new_articles);
                                                    let new_count = added_articles.len();
                                                    
                                                    if new_count > 0 {
                                                        log::info!("订阅源 {} 有 {} 篇新文章", feed.title, new_count);
                                                        
                                                        // 按发布时间倒序排列，取最新的文章
                                                        let mut sorted_articles = added_articles; // 直接使用，无需clone
                                                        sorted_articles.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
                                                        
                                                        // 将所有新文章标题添加到通知列表
                                                        let mut new_article_titles = Vec::new();
                                                        for article in sorted_articles.iter() {
                                                            new_article_titles.push(article.title.clone());
                                                        }
                                                    
                                                        // 发送新文章通知
                                                        article_processor::send_new_articles_notification(
                                                            Some(notification_manager.clone()),
                                                            &feed.title,
                                                            &new_article_titles,
                                                            feed.enable_notification
                                                        );
                                                    }
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            log::error!("Failed to fetch RSS for feed {}: {}", feed.title, e);
                                        }}
                            }
                        
                        // 发送全部更新完成消息，触发文章列表重新加载
                        let _ = ui_tx.send(UiMessage::AllFeedsRefreshed);
                        });
                        ui.close();
                    }
                });
                
                ui.menu_button("设置", |ui| {
                    // 主题设置
                    ui.menu_button("主题", |ui| {
                        if ui.button("亮色主题").clicked() {
                            self.is_dark_mode = false;
                            ctx.set_visuals(egui::Visuals::light());
                            self.config.theme = "light".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save theme setting: {}", e);
                            }
                            ui.close();
                        }
                        if ui.button("暗色主题").clicked() {
                            self.is_dark_mode = true;
                            ctx.set_visuals(egui::Visuals::dark());
                            self.config.theme = "dark".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save theme setting: {}", e);
                            }
                            ui.close();
                        }
                        if ui.button("跟随系统").clicked() {
                            let system_theme = ctx.style().visuals.dark_mode;
                            self.is_dark_mode = system_theme;
                            ctx.set_visuals(if system_theme {
                                egui::Visuals::dark()
                            } else {
                                egui::Visuals::light()
                            });
                            self.config.theme = "system".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save theme setting: {}", e);
                            }
                             ui.close();
                        }
                    });
                    
                    // 时区设置
                    ui.menu_button("时区设置", |ui| {
                        if ui.button("UTC").clicked() {
                            self.config.timezone = "UTC".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save timezone setting: {}", e);
                            }
                            // 请求UI重新绘制，使时区设置即时生效
                            ctx.request_repaint();
                            ui.close();
                        }
                    
                        if ui.button("Asia/Shanghai").clicked() {
                            self.config.timezone = "Asia/Shanghai".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save timezone setting: {}", e);
                            }
                            // 请求UI重新绘制，使时区设置即时生效
                            ctx.request_repaint();
                            ui.close();
                        }
                        if ui.button("Asia/Tokyo").clicked() {
                            self.config.timezone = "Asia/Tokyo".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save timezone setting: {}", e);
                            }
                            // 请求UI重新绘制，使时区设置即时生效
                            ctx.request_repaint();
                            ui.close();
                        }
                        if ui.button("America/New_York").clicked() {
                            self.config.timezone = "America/New_York".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save timezone setting: {}", e);
                            }
                            // 请求UI重新绘制，使时区设置即时生效
                            ctx.request_repaint();
                            ui.close();
                        }
                        if ui.button("Europe/London").clicked() {
                            self.config.timezone = "Europe/London".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save timezone setting: {}", e);
                            }
                            // 请求UI重新绘制，使时区设置即时生效
                            ctx.request_repaint();
                            ui.close();
                        }
                    });
                    
                    // 字体大小设置
                    let mut font_size = self.font_size;
                    if ui.add(egui::Slider::new(&mut font_size, 10.0..=24.0).text("字体大小")).changed() {
                        self.font_size = font_size;
                        self.config.font_size = font_size;
                        if let Err(e) = self.config.save() {
                            log::error!("Failed to save font size setting: {}", e);
                        }
                    }
                    
                    // 系统托盘设置
                    if ui.checkbox(&mut self.config.show_tray_icon, "启用系统托盘").changed()
                        && let Err(e) = self.config.save() {
                            log::error!("Failed to save tray setting: {}", e);
                        }
                    
                    // 控制台窗口设置（仅Windows）
                    #[cfg(target_os = "windows")]
                    {
                        if ui.checkbox(&mut self.config.show_console, "显示控制台窗口").changed() {
                            // 立即应用控制台窗口显示设置
                            fn set_console_visible(visible: bool) {
                                // use std::ptr;
                                use winapi::um::wincon::GetConsoleWindow;
                                use winapi::um::winuser::{SW_HIDE, SW_SHOW, ShowWindow};
                                
                                unsafe {
                                    let console_window = GetConsoleWindow();
                                    if !console_window.is_null() {
                                        if visible {
                                            ShowWindow(console_window, SW_SHOW);
                                        } else {
                                            ShowWindow(console_window, SW_HIDE);
                                        }
                                    }
                                }
                            }
                            
                            set_console_visible(self.config.show_console);
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save console setting: {}", e);
                            }
                        }
                    }
                    
                    // 搜索方式设置
                    ui.menu_button("搜索方式", |ui| {
                        ui.label("搜索方式设置（重启应用后生效）");
                        ui.separator();
                        
                        if ui.radio(self.config.search_mode == "index_search", "建立搜索索引").clicked() {
                            self.config.search_mode = "index_search".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save search mode setting: {}", e);
                            }
                        }
                        
                        if ui.radio(self.config.search_mode == "direct_search", "直接搜索").clicked() {
                            self.config.search_mode = "direct_search".to_string();
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save search mode setting: {}", e);
                            }
                        }
                        
                        ui.separator();
                        ui.label(egui::RichText::new("建立搜索索引：启动时建立索引，搜索速度快，但占用更多内存")
                            .size(self.font_size * 0.8)
                            .weak());
                        ui.label(egui::RichText::new("直接搜索：不建立索引，启动速度快，占用内存少，但搜索速度稍慢")
                            .size(self.font_size * 0.8)
                            .weak());
                    });
                    
                    // 通知设置
                    ui.menu_button("通知设置", |ui| {
                        if ui.checkbox(&mut self.config.enable_notifications, "启用新文章通知").changed() {
                            // 更新通知管理器设置
                            let notification_manager = self.notification_manager.clone();
                            let enable_notifications = self.config.enable_notifications;
                            let max_notifications = self.config.max_notifications;
                            let notification_timeout_ms = self.config.notification_timeout_ms;
                            tokio::spawn(async move {
                                notification_manager.lock().await.update_config(
                                    enable_notifications,
                                    max_notifications,
                                    notification_timeout_ms
                                );
                            });
                            
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save notification setting: {}", e);
                            }
                        }
                        
                        ui.add(egui::Slider::new(&mut self.config.max_notifications, 1..=10).text("最大通知数量"));
                        ui.add(egui::Slider::new(&mut self.config.notification_timeout_ms, 1000..=10000).text("通知显示时间（毫秒）"));
                        
                        if ui.button("保存通知设置").clicked() {
                            // 更新通知管理器设置
                            let notification_manager = self.notification_manager.clone();
                            let config = self.config.clone();
                            tokio::spawn(async move {
                                notification_manager.lock().await.update_config(
                                    config.enable_notifications,
                                    config.max_notifications,
                                    config.notification_timeout_ms
                                );
                            });
                            
                            if let Err(e) = self.config.save() {
                                log::error!("Failed to save notification settings: {}", e);
                            }
                            ui.close();
                        }
                    });
                    
                    // AI设置
                    ui.separator();
                    if ui.button("AI设置").clicked() {
                        // 将当前配置的值复制到AI设置窗口的输入字段
                        self.ai_settings_api_url = self.config.ai_api_url.clone();
                        self.ai_settings_api_key = self.config.ai_api_key.clone();
                        self.ai_settings_model_name = self.config.ai_model_name.clone();
                        
                        self.show_ai_settings_dialog = true;
                    }
                    
                    // 自动更新设置
                    ui.separator();
                    ui.label("自动更新设置:");
                    
                    // 启用/禁用自动更新
                    let mut auto_update_enabled = self.auto_update_enabled;
                    if ui.checkbox(&mut auto_update_enabled, "启用自动更新").changed() {
                        self.auto_update_enabled = auto_update_enabled;
                        // 保存到数据库
                        let storage = self.storage.clone();
                        let ui_tx = self.ui_tx.clone();
                        tokio::spawn(async move {
                            let mut storage_lock = storage.lock().await;
                            if let Err(e) = storage_lock.set_auto_update_enabled(auto_update_enabled).await {
                                log::error!("保存自动更新状态失败: {}", e);
                                if let Err(e) = ui_tx.send(UiMessage::Error(format!("保存设置失败: {}", e))) {
                                    log::error!("发送错误消息失败: {}", e);
                                }
                            } else {
                                // 发送消息更新UI状态
                                if let Err(e) = ui_tx.send(UiMessage::AutoUpdateEnabledChanged(auto_update_enabled)) {
                                    log::error!("发送自动更新状态变更消息失败: {}", e);
                                }
                            }
                        });
                    }
                    
                    // 设置更新间隔（只有启用了自动更新才显示）
                    if self.auto_update_enabled {
                        let mut update_interval = self.auto_update_interval;
                        if ui.add(egui::Slider::new(&mut update_interval, 1..=120).text("更新间隔（分钟）")).changed() {
                            self.auto_update_interval = update_interval;
                            // 保存到数据库
                            let storage = self.storage.clone();
                            let ui_tx = self.ui_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = storage.lock().await.set_update_interval(update_interval).await {
                                    log::error!("保存更新间隔失败: {}", e);
                                    if let Err(e) = ui_tx.send(UiMessage::Error(format!("保存设置失败: {}", e))) {
                                        log::error!("发送错误消息失败: {}", e);
                                    }
                                } else {
                                    // 发送消息更新UI状态和自动更新任务
                                    if let Err(e) = ui_tx.send(UiMessage::UpdateIntervalLoaded(update_interval)) {
                                        log::error!("发送更新间隔消息失败: {}", e);
                                    }
                                }
                            });
                        }
                    }
                });
            });
        });

        // 定义背景色变量
        let _central_bg_color = if self.is_dark_mode {
            egui::Color32::from_rgb(40, 40, 40) // 深色主题中央面板背景
        } else {
            egui::Color32::WHITE // 浅色主题中央面板背景
        };

        let sidebar_bg_color = if self.is_dark_mode {
            egui::Color32::from_rgb(30, 30, 30) // 深色主题侧边栏背景
        } else {
            egui::Color32::from_rgb(240, 240, 240) // 浅色主题侧边栏背景
        };

        // 左侧订阅源面板 - 设置固定宽度和明确的背景色
        egui::SidePanel::left("feeds_panel")
            .min_width(200.0)
            .max_width(300.0)
            .frame(egui::Frame::default().fill(sidebar_bg_color))
            .show(ctx, |ui| {
                ui.heading("订阅源");
                ui.separator();

                // 添加"全部"选项，使用特殊ID -1
                let all_selected = self.selected_feed_id == Some(-1);
                let all_response = ui.selectable_label(all_selected, "📋 全部");

                // 处理"全部"选项的点击事件
                if all_response.clicked() {
                    self.selected_feed_id = Some(-1);
                    self.selected_article_id = None;

                    // 加载所有文章
                    let storage = self.storage.clone();
                    let ui_tx = self.ui_tx.clone();
                    tokio::spawn(async move {
                        // 使用StorageManager的get_all_articles方法
                        let storage_lock = storage.lock().await;
                        match storage_lock.get_all_articles().await {
                            Ok(articles) => {
                                if let Err(e) = ui_tx.send(UiMessage::ArticlesLoaded(articles)) {
                                    log::error!("发送文章加载消息失败: {}", e);
                                }
                            }
                            Err(e) => {
                                log::error!("加载所有文章失败: {}", e);
                                if let Err(ui_error) =
                                    ui_tx.send(UiMessage::Error(format!("加载所有文章失败: {}", e)))
                                {
                                    log::error!("发送错误消息失败: {}", ui_error);
                                }
                            }
                        }
                    });
                }
                ui.separator();

                // 计算当前feeds的哈希值，用于检测是否变化
                let current_hash = self.calculate_feeds_hash();

                // 只有在feeds发生变化时才重新计算分组
                if self.feeds_hash != Some(current_hash) {
                    // 清除旧的缓存
                    self.cached_groups.clear();

                    // 重新计算分组
                    for feed in &self.feeds {
                        self.cached_groups
                            .entry(feed.group.clone())
                            .or_default()
                            .push(feed.clone());
                    }

                    // 对每个分组内的订阅源按标题排序
                    for feeds in self.cached_groups.values_mut() {
                        feeds.sort_by(|a, b| a.title.cmp(&b.title));
                    }

                    // 更新哈希值
                    self.feeds_hash = Some(current_hash);
                }

                // 显示所有订阅源
                if self.cached_groups.contains_key("") {
                    for feed in self.cached_groups.get("").unwrap_or(&Vec::new()) {
                        // 创建可选择的标签
                        let response = ui
                            .selectable_label(self.selected_feed_id == Some(feed.id), &feed.title);

                        // 处理左键点击
                        if response.clicked() {
                            self.selected_feed_id = Some(feed.id);
                            self.selected_article_id = None;

                            // 加载该订阅源的文章
                            // 克隆必要的参数而不是整个App实例
                            let storage = self.storage.clone();
                            let ui_tx = self.ui_tx.clone();
                            let feed_id = feed.id;
                            tokio::spawn(async move {
                                // 加载文章的逻辑
                                if let Err(e) =
                                    get_articles_by_feed_async(storage, feed_id, ui_tx).await
                                {
                                    log::error!("Failed to load articles: {}", e);
                                }
                            });
                        }

                        // 处理右键点击，显示上下文菜单
                        response.context_menu(|ui| {
                            // 编辑订阅源
                            if ui.button("编辑订阅源").clicked() {
                                // 这里将实现编辑订阅源的逻辑
                                // 先保存当前要编辑的订阅源信息
                                self.selected_feed_id = Some(feed.id);
                                self.edit_feed_id = Some(feed.id);
                                self.edit_feed_title = feed.title.clone();
                                self.edit_feed_url = feed.url.clone();
                                self.edit_feed_group = feed.group.clone();
                                self.edit_feed_auto_update = feed.auto_update;
                                self.edit_feed_enable_notification = feed.enable_notification;
                                self.edit_feed_ai_auto_translate = feed.ai_auto_translate;
                                self.show_edit_feed_dialog = true;
                                ui.close();
                            }

                            // 删除订阅源
                            if ui.button("删除订阅源").clicked() {
                                // 克隆必要的参数
                                let storage = self.storage.clone();
                                let ui_tx = self.ui_tx.clone();
                                let feed_id = feed.id;
                                let feed_title = feed.title.clone();
                                let is_selected = self.selected_feed_id == Some(feed_id);

                                tokio::spawn(async move {
                                    // 开始删除操作
                                    let mut storage_guard = storage.lock().await;

                                    // 先删除该订阅源的所有文章
                                    if let Err(e) =
                                        storage_guard.delete_feed_articles(feed_id).await
                                    {
                                        log::error!("删除订阅源 {} 的文章失败: {}", feed_title, e);
                                        let _ = ui_tx.send(UiMessage::Error(format!(
                                            "删除订阅源文章失败: {}",
                                            e
                                        )));
                                        return;
                                    }

                                    // 然后删除订阅源本身
                                    if let Err(e) = storage_guard.delete_feed(feed_id).await {
                                        log::error!("删除订阅源 {} 失败: {}", feed_title, e);
                                        let _ = ui_tx.send(UiMessage::Error(format!(
                                            "删除订阅源失败: {}",
                                            e
                                        )));
                                        return;
                                    }

                                    log::info!("成功删除订阅源: {}", feed_title);

                                    // 重新加载所有订阅源
                                    if let Ok(feeds) = storage_guard.get_all_feeds().await {
                                        // 发送订阅源更新消息
                                        let _ = ui_tx.send(UiMessage::FeedsLoaded(feeds));

                                        // 如果删除的是当前选中的订阅源，清除文章和选中状态
                                        if is_selected {
                                            let _ =
                                                ui_tx.send(UiMessage::ArticlesLoaded(Vec::new()));
                                            let _ = ui_tx.send(UiMessage::ClearSelectedFeed);
                                        }
                                    }

                                    let _ = ui_tx.send(UiMessage::StatusMessage(format!(
                                        "成功删除订阅源: {}",
                                        feed_title
                                    )));
                                });
                            }
                        });
                    }
                }

                // 按分组显示订阅源
                for (group, feeds) in &self.cached_groups {
                    if !group.is_empty() {
                        ui.collapsing(format!("📁 {}", group), |ui| {
                            // 编辑分组按钮
                            if ui.button("✏️").clicked() {
                                // 设置编辑分组状态
                                self.editing_group_name = group.clone();
                                self.new_group_name = group.clone();
                                self.show_edit_group_dialog = true;
                            }

                            ui.separator();
                            for feed in feeds {
                                // 创建可选择的标签
                                let response = ui.selectable_label(
                                    self.selected_feed_id == Some(feed.id),
                                    &feed.title,
                                );

                                // 处理左键点击
                                if response.clicked() {
                                    self.selected_feed_id = Some(feed.id);
                                    self.selected_article_id = None;

                                    // 加载该订阅源的文章
                                    let storage = self.storage.clone();
                                    let ui_tx = self.ui_tx.clone();
                                    let feed_id = feed.id;
                                    tokio::spawn(async move {
                                        // 通过消息传递机制更新UI
                                        if let Err(e) =
                                            get_articles_by_feed_async(storage, feed_id, ui_tx)
                                                .await
                                        {
                                            log::error!("Failed to load articles: {}", e);
                                        }
                                    });
                                    // 由于UI回调不能是异步的，我们使用一个简单的方法
                                    // 先设置刷新标志，然后在异步任务中更新
                                    self.is_refreshing = true;
                                    ctx.request_repaint(); // 请求重新绘制UI
                                }

                                // 处理右键点击，显示上下文菜单
                                response.context_menu(|ui| {
                                    // 编辑订阅源
                                    if ui.button("编辑订阅源").clicked() {
                                        // 保存当前要编辑的订阅源信息
                                        self.selected_feed_id = Some(feed.id);
                                        self.edit_feed_id = Some(feed.id);
                                        self.edit_feed_title = feed.title.clone();
                                        self.edit_feed_url = feed.url.clone();
                                        self.edit_feed_group = feed.group.clone();
                                        self.edit_feed_auto_update = feed.auto_update;
                                        self.edit_feed_enable_notification = feed.enable_notification;
                                        self.edit_feed_ai_auto_translate = feed.ai_auto_translate;
                                        self.show_edit_feed_dialog = true;
                                        // 使用close_menu的替代方法
                                        ui.close();
                                    }

                                    // 删除订阅源
                                    if ui.button("删除订阅源").clicked() {
                                        // 克隆必要的参数
                                        let storage = self.storage.clone();
                                        let ui_tx = self.ui_tx.clone();
                                        let feed_id = feed.id;
                                        let feed_title = feed.title.clone();
                                        let is_selected = self.selected_feed_id == Some(feed_id);

                                        tokio::spawn(async move {
                                            // 开始删除操作
                                            let mut storage_guard = storage.lock().await;

                                            // 先删除该订阅源的所有文章
                                            if let Err(e) =
                                                storage_guard.delete_feed_articles(feed_id).await
                                            {
                                                log::error!(
                                                    "删除订阅源 {} 的文章失败: {}",
                                                    feed_title,
                                                    e
                                                );
                                                let _ = ui_tx.send(UiMessage::Error(format!(
                                                    "删除订阅源文章失败: {}",
                                                    e
                                                )));
                                                return;
                                            }

                                            // 然后删除订阅源本身
                                            if let Err(e) = storage_guard.delete_feed(feed_id).await
                                            {
                                                log::error!(
                                                    "删除订阅源 {} 失败: {}",
                                                    feed_title,
                                                    e
                                                );
                                                let _ = ui_tx.send(UiMessage::Error(format!(
                                                    "删除订阅源失败: {}",
                                                    e
                                                )));
                                                return;
                                            }

                                            log::info!("成功删除订阅源: {}", feed_title);

                                            // 重新加载所有订阅源
                                            if let Ok(feeds) = storage_guard.get_all_feeds().await {
                                                // 发送订阅源更新消息
                                                let _ = ui_tx.send(UiMessage::FeedsLoaded(feeds));

                                                // 如果删除的是当前选中的订阅源，清除文章和选中状态
                                                if is_selected {
                                                    let _ = ui_tx.send(UiMessage::ArticlesLoaded(
                                                        Vec::new(),
                                                    ));
                                                    let _ =
                                                        ui_tx.send(UiMessage::ClearSelectedFeed);
                                                }
                                            }

                                            let _ = ui_tx.send(UiMessage::StatusMessage(format!(
                                                "成功删除订阅源: {}",
                                                feed_title
                                            )));
                                        });
                                    }
                                });
                            }
                        });
                    }
                }

                ui.add_space(20.0);
                ui.separator();

                // 添加新订阅源按钮
                if ui.button("+ 添加订阅源").clicked() {
                    self.show_add_feed_dialog = true;
                }
            });

        // 为所有面板设置明确的背景色以解决渲染问题
        let central_bg_color = if self.is_dark_mode {
            egui::Color32::from_rgb(40, 40, 40) // 深色主题中央面板背景
        } else {
            egui::Color32::WHITE // 浅色主题中央面板背景
        };

        let sidebar_bg_color = if self.is_dark_mode {
            egui::Color32::from_rgb(30, 30, 30) // 深色主题侧边栏背景
        } else {
            egui::Color32::from_rgb(240, 240, 240) // 浅色主题侧边栏背景
        };

        // 强制设置中央面板背景，确保不会出现黑屏
        // 使用更明确的Frame设置，确保渲染正确
        egui::CentralPanel::default()
            .frame(egui::Frame::default()
                .fill(central_bg_color)
                .stroke(egui::Stroke::new(1.0, egui::Color32::GRAY)))
            .show(ctx, |ui| {
            // 调试信息
            ui.label("文章列表区域");
            ui.label(format!("当前选中的订阅源ID: {:?}", self.selected_feed_id));
            ui.label(format!("文章数量: {}", self.articles.len()));
            
            if self.selected_feed_id.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("请选择一个订阅源");
                });
                return;
            }
            
            ui.heading("文章列表");
            ui.separator();
            
            // 搜索框
            ui.horizontal(|ui| {
                ui.label("搜索:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_input)
                        .hint_text("输入关键字按回城搜索..")
                        .desired_width(200.0)
                );
                
                // 清除按钮
                if !self.search_input.is_empty() && ui.button("清除").clicked() {
                    self.search_input.clear();
                    // 立即执行搜索（会触发空搜索逻辑，重新加载文章）
                    self.execute_search();
                }
                
                // 显示搜索性能指标
                if let Some(duration) = &self.last_search_time {
                    let ms = duration.as_millis();
                    let color = if ms < 500 {
                        egui::Color32::GREEN
                    } else if ms < 1000 {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::RED
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("搜索耗时: {}ms", ms)).color(color));
                    });
                }
                
                // 当搜索框内容改变时，请求在下一帧检查是否需要执行搜索（防抖）
                if response.changed() {
                    ctx.request_repaint();
                }
                
                // 按下回车时立即执行搜索，不等待防抖
                if response.lost_focus() && ui.input_mut(|i| i.key_pressed(egui::Key::Enter)) {
                    // 立即执行搜索，无论是空还是非空输入
                    self.execute_search();
                    // 取消防抖定时器，避免重复执行
                    self.search_debounce_timer = None;
                }
                
                // 处理搜索防抖
                self.handle_search_debounce(ctx);
            });
            
            // 文章过滤和排序选项
            ui.horizontal(|ui| {
                // 当复选框状态发生变化时，请求重新绘制UI以立即应用过滤
                let mut show_only_unread_changed = false;
                let mut show_only_starred_changed = false;
                let mut sort_options_changed = false;
                
                if ui.checkbox(&mut self.show_only_unread, "仅显示未读").changed() {
                    show_only_unread_changed = true;
                }
                
                if ui.checkbox(&mut self.show_only_starred, "仅显示收藏").changed() {
                    show_only_starred_changed = true;
                }
                
                ui.separator();
                
                ui.menu_button("排序", |ui| {
                    if ui.checkbox(&mut self.sort_by_date, "按日期").changed() {
                        sort_options_changed = true;
                    }
                    if ui.checkbox(&mut !self.sort_by_date, "按标题").changed() {
                        self.sort_by_date = false; // 确保状态一致性
                        sort_options_changed = true;
                    }
                    ui.separator();
                    if ui.checkbox(&mut self.sort_descending, "降序排列").changed() {
                        sort_options_changed = true;
                    }
                });
                
                // 如果任何过滤或排序选项发生变化，请求重新绘制UI
                if show_only_unread_changed || show_only_starred_changed || sort_options_changed {
                    ctx.request_repaint();
                }
                
                ui.separator();
                
                if ui.button("刷新").clicked()
                    && let Some(feed_id) = self.selected_feed_id {
                        // 设置刷新标志
                        self.is_refreshing = true;
                        
                        // 克隆必要的参数而不是整个App实例
                        let storage = self.storage.clone();
                        let rss_fetcher = self.rss_fetcher.clone();
                        let notification_manager = self.notification_manager.clone();
                        let ui_tx = self.ui_tx.clone();
                        let ai_client = self.ai_client.clone().map(Arc::new);
                        tokio::spawn(async move {
                            if feed_id == -1 {
                                // 刷新所有订阅源的逻辑
                                let feeds = {
                                    let storage_lock = storage.lock().await;
                                    match storage_lock.get_all_feeds().await {
                                        Ok(feeds) => feeds,
                                        Err(e) => {
                                            log::error!("Failed to get all feeds: {}", e);
                                            // 发送错误消息，重置刷新标志
                                            let _ = ui_tx.send(UiMessage::Error(format!("获取订阅源失败: {}", e)));
                                            return;
                                        }
                                    }
                                };
                                
                                for feed in feeds {
                                    let storage_clone = storage.clone();
                                    let rss_fetcher_clone = rss_fetcher.clone();
                                    let notification_manager_clone = notification_manager.clone();
                                    let ui_tx_clone = ui_tx.clone();
                                    let ai_client_clone = ai_client.clone();
                                    if let Err(e) = refresh_single_feed_async(storage_clone, rss_fetcher_clone, feed.id, Some(notification_manager_clone), Some(ui_tx_clone), ai_client_clone).await {
                                        log::error!("Failed to refresh feed {}: {}", feed.id, e);
                                    }
                                }
                                
                                // 刷新完成后发送AllFeedsRefreshed消息，重置刷新标志
                                let _ = ui_tx.send(UiMessage::AllFeedsRefreshed);
                            } else {
                                // 刷新单个订阅源的逻辑
                                let ui_tx_clone = ui_tx.clone();
                                if let Err(e) = refresh_single_feed_async(storage, rss_fetcher, feed_id, Some(notification_manager), Some(ui_tx_clone), ai_client).await {
                                    log::error!("Failed to refresh feed: {}", e);
                                    // 发送错误消息，重置刷新标志
                                    let _ = ui_tx.send(UiMessage::Error(format!("刷新订阅源失败: {}", e)));
                                }
                            }
                        });
                    }
                
                if ui.button("全部已读").clicked()
                    && let Some(feed_id) = self.selected_feed_id {
                        // 克隆必要的参数而不是整个App实例
                        let storage = self.storage.clone();
                        let ui_tx = self.ui_tx.clone();
                        tokio::spawn(async move {
                            // 标记所有文章为已读的逻辑
                            // 如果feed_id为-1，表示"全部"选项，标记所有文章为已读
                            // 否则只标记指定订阅源的文章
                            let target_feed_id = if feed_id == -1 { None } else { Some(feed_id) };
                            if let Err(e) = mark_all_as_read_async(storage, target_feed_id, ui_tx).await {
                                log::error!("Failed to mark all articles as read: {}", e);
                            }
                        });
                    }
                
                if ui.button("全部删除").clicked()
                    && let Some(feed_id) = self.selected_feed_id {
                        // 克隆必要的参数而不是整个App实例
                        let storage = self.storage.clone();
                        let search_manager = self.search_manager.clone();
                        let ui_tx_outer = self.ui_tx.clone();
                        let ui_tx_inner = self.ui_tx.clone();
                        tokio::spawn(async move {
                            // 删除所有文章的逻辑
                            // 如果feed_id为-1，表示"全部"选项，删除所有文章
                            // 否则只删除指定订阅源的文章
                            let target_feed_id = if feed_id == -1 { None } else { Some(feed_id) };
                            if let Err(e) = delete_all_articles_async(storage, search_manager, target_feed_id, ui_tx_inner).await {
                                log::error!("删除文章失败: {}", e);
                            } else {
                                // 删除成功后重新加载文章列表
                                let _ = ui_tx_outer.send(UiMessage::ReloadArticles);
                            }
                        });
                    }

                if ui.button("和AI聊天").clicked() {
                    // 显示AI对话窗口
                    self.show_ai_chat_window = true;
                    
                    // 确保AI流式相关状态被正确初始化
                    self.is_ai_streaming = false;
                    self.current_ai_stream_response = String::new();
                    self.is_sending_message = false;
                    
                    // 创建或获取AI聊天会话
                    if self.ai_chat_session.is_none() {
                        if let Some(client) = &self.ai_client {
                            let mut session = AIChatSession::new(client.clone());
                            // 设置系统提示
                            session.set_system_prompt("你是一个有用的助手。请根据提供的文章内容，给出详细、准确的回答。");
                            // 加载聊天历史记录
                            if let Err(e) = session.load_chat_history() {
                                log::error!("加载聊天历史记录失败: {}", e);
                            }
                            self.ai_chat_session = Some(session);
                        }
                    } else {
                        // 加载聊天历史记录
                        if let Some(session) = self.ai_chat_session.as_mut()
                            && let Err(e) = session.load_chat_history() {
                                log::error!("加载聊天历史记录失败: {}", e);
                            }
                    }
                }

             


            });
            
            // 显示搜索状态或结果数量
            if self.is_searching {
                ui.label("正在搜索中...");
            } else if !self.search_results.is_empty() {
                ui.label(format!("搜索结果: {} 篇文章", self.search_results.len()));
            }
            
            // 直接计算过滤和排序后的文章列表，取消缓存
            let articles_to_display = if !self.search_results.is_empty() && self.is_search_mode {
                // 显示搜索结果，对结果应用过滤和排序
                self.filter_and_sort_articles(&self.search_results)
            } else {
                // 显示正常文章列表，直接计算过滤和排序后的结果
                self.filter_and_sort_articles(&self.articles)
            };
            
            // 分页处理
            let total_articles = articles_to_display.len();
            let total_pages = if total_articles == 0 {
                1
            } else {
                total_articles.div_ceil(self.page_size)
            };
            
            // 确保当前页码有效
            if self.current_page > total_pages {
                self.current_page = total_pages;
            }
            if self.current_page < 1 {
                self.current_page = 1;
            }
            
            // 计算当前页的文章范围
            let start_idx = (self.current_page - 1) * self.page_size;
            let end_idx = std::cmp::min(start_idx + self.page_size, total_articles);
            let current_page_articles = &articles_to_display[start_idx..end_idx];
            
            // 键盘导航处理
            let input = ui.input_mut(|i| i.clone());
            let has_articles = !articles_to_display.is_empty();
            
            // 移除焦点检查，直接处理键盘事件
            // 分页导航（左右方向键）
            if has_articles {
                if input.key_pressed(egui::Key::ArrowLeft) && self.current_page > 1 {
                    self.current_page -= 1;
                    // 切换页面后，默认选中第一篇文章
                    let new_start_idx = (self.current_page - 1) * self.page_size;
                    let new_end_idx = std::cmp::min(new_start_idx + self.page_size, total_articles);
                    let new_page_articles = &articles_to_display[new_start_idx..new_end_idx];
                    if !new_page_articles.is_empty() {
                        self.selected_article_id = Some(new_page_articles[0].id);
                        // 自动标记为已读
                        if let Some(article) = new_page_articles.iter().find(|a| a.id == self.selected_article_id.unwrap_or(-1))
                            && !article.is_read {
                                let storage = self.storage.clone();
                                let article_id = article.id;
                                let ui_tx = self.ui_tx.clone();
                                tokio::spawn(async move { if let Err(e) = mark_article_as_read_async(storage, article_id, ui_tx).await { log::error!("Failed to mark article as read: {}", e); } });
                            }
                    }
                } else if input.key_pressed(egui::Key::ArrowRight) && self.current_page < total_pages {
                    self.current_page += 1;
                    // 切换页面后，默认选中第一篇文章
                    let next_start_idx = (self.current_page - 1) * self.page_size;
                    let next_end_idx = std::cmp::min(next_start_idx + self.page_size, total_articles);
                    let next_page_articles = &articles_to_display[next_start_idx..next_end_idx];
                    if !next_page_articles.is_empty() {
                        self.selected_article_id = Some(next_page_articles[0].id);
                        // 自动标记为已读
                        if let Some(article) = next_page_articles.iter().find(|a| a.id == self.selected_article_id.unwrap_or(-1))
                            && !article.is_read {
                                let storage = self.storage.clone();
                                let article_id = article.id;
                                let ui_tx = self.ui_tx.clone();
                                tokio::spawn(async move { if let Err(e) = mark_article_as_read_async(storage, article_id, ui_tx).await { log::error!("Failed to mark article as read: {}", e); } });
                            }
                    }
                }
            }
            
            // 文章选择（上下方向键）
            if has_articles && !current_page_articles.is_empty()
                && (input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowDown)) {
                    // 找到当前选中文章在当前页的索引
                    let current_index = if let Some(selected_id) = self.selected_article_id {
                        current_page_articles.iter().position(|a| a.id == selected_id).unwrap_or(0)
                    } else {
                        // 如果没有选中文章，默认选中第一篇
                        self.selected_article_id = Some(current_page_articles[0].id);
                        0
                    };
                    
                    // 计算新的索引
                    let new_index = if input.key_pressed(egui::Key::ArrowUp) {
                        // 向上导航
                        if current_index > 0 {
                            current_index - 1
                        } else if self.current_page > 1 {
                            // 如果是当前页的第一篇文章，切换到上一页的最后一篇
                            self.current_page -= 1;
                            // 计算上一页的最后一篇文章索引
                            let prev_start_idx = (self.current_page - 1) * self.page_size;
                            let prev_end_idx = std::cmp::min(prev_start_idx + self.page_size, total_articles);
                            let prev_page_articles = &articles_to_display[prev_start_idx..prev_end_idx];
                            prev_page_articles.len() - 1
                        } else {
                            // 已经是第一页的第一篇文章，保持不变
                            0
                        }
                    } else {
                        // 向下导航
                        if current_index < current_page_articles.len() - 1 {
                            current_index + 1
                        } else if self.current_page < total_pages {
                            // 如果是当前页的最后一篇文章，切换到下一页的第一篇
                            self.current_page += 1;
                            0
                        } else {
                            // 已经是最后一页的最后一篇文章，保持不变
                            current_index
                        }
                    };
                    
                    // 获取新页面的文章列表
                    let new_start_idx = (self.current_page - 1) * self.page_size;
                    let new_end_idx = std::cmp::min(new_start_idx + self.page_size, total_articles);
                    let new_page_articles = &articles_to_display[new_start_idx..new_end_idx];
                    
                    // 更新选中的文章
                    if !new_page_articles.is_empty() && new_index < new_page_articles.len() {
                        self.selected_article_id = Some(new_page_articles[new_index].id);
                        // 自动标记为已读
                        if let Some(article) = new_page_articles.iter().find(|a| a.id == self.selected_article_id.unwrap_or(-1))
                            && !article.is_read {
                                let storage = self.storage.clone();
                                let article_id = article.id;
                                let ui_tx = self.ui_tx.clone();
                                tokio::spawn(async move { if let Err(e) = mark_article_as_read_async(storage, article_id, ui_tx).await { log::error!("Failed to mark article as read: {}", e); } });
                            }
                    }
                }
            
            // 检查是否正在建立搜索索引
            if self.is_indexing {
                // 显示加载动画
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("正在建立搜索索引，请稍候...");
                    ui.label(format!("已处理: {} / {} 篇文章", self.indexing_progress, self.indexing_total));
                });
            } else {
                // 批量操作工具栏
                ui.horizontal(|ui| {
                    if ui.button("全选").clicked() {
                        for article in &self.articles {
                            self.selected_articles.insert(article.id);
                        }
                    }
                    
                    if ui.button("取消全选").clicked() {
                        self.selected_articles.clear();
                    }
                    
                    if ui.button("批量发送给AI").clicked() {
                        if self.selected_articles.is_empty() {
                            self.status_message = "请先选择要发送的文章".to_string();
                        } else {
                            // 生成预览内容
                            self.update_ai_send_preview();
                            // 显示AI发送对话框
                            self.show_ai_send_dialog = true;
                        }
                    }
                    
                    if ui.button("批量已读").clicked() {
                        if self.selected_articles.is_empty() {
                            self.status_message = "请先选择要操作的文章".to_string();
                        } else {
                            let article_ids: Vec<i64> = self.selected_articles.iter().copied().collect();
                            let storage_clone = Arc::clone(&self.storage);
                            let ui_tx_clone = self.ui_tx.clone();
                            
                            tokio::spawn(async move {
                                let mut storage = storage_clone.lock().await;
                                if storage.mark_articles_as_read(&article_ids).await.is_ok() {
                                    let _ = ui_tx_clone.send(UiMessage::StatusMessage("文章已批量标记为已读".to_string()));
                                    let _ = ui_tx_clone.send(UiMessage::ReloadArticles);
                                }
                            });
                        }
                    }
                    
                    if ui.button("批量删除").clicked() {
                        if self.selected_articles.is_empty() {
                            self.status_message = "请先选择要删除的文章".to_string();
                        } else {
                            let article_ids: Vec<i64> = self.selected_articles.iter().copied().collect();
                            let storage_clone = Arc::clone(&self.storage);
                            let ui_tx_clone = self.ui_tx.clone();
                            
                            tokio::spawn(async move {
                                let mut storage = storage_clone.lock().await;
                                if storage.delete_articles(&article_ids).await.is_ok() {
                                    let _ = ui_tx_clone.send(UiMessage::StatusMessage("文章已批量删除".to_string()));
                                    let _ = ui_tx_clone.send(UiMessage::ReloadArticles);
                                }
                            });
                        }
                    }
                    
                    ui.label(format!("已选择: {} 篇文章", self.selected_articles.len()));
                });
                
                ui.separator();
                
                // 文章列表 - 只显示当前页的文章
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // 使用迭代器遍历当前页的文章，添加数字索引
                    for (index, article) in current_page_articles.iter().enumerate() {
                        // 计算全局索引（包含当前页偏移）
                        let global_index = start_idx + index + 1;
                        
                        // 显示收藏标记、数字索引和订阅源名称
                        // 过滤掉标题中的换行符，防止分页栏目被挤出视图
                        let clean_title = article.title.replace(['\n', '\r'], "");
                        let title_text = if article.is_starred {
                            format!("{} ⭐ {} ({})", global_index, clean_title, article.source)
                        } else {
                            format!("{} {} ({})", global_index, clean_title, article.source)
                        };
                        
                        // 根据文章状态设置文本颜色
                        let title = if article.is_read {
                            // 已读文章显示为灰色
                            egui::RichText::new(title_text).color(egui::Color32::GRAY)
                        } else {
                            // 未读文章使用正常颜色
                            egui::RichText::new(title_text)
                        };
                        
                        // 文章行布局
                        ui.horizontal(|ui| {
                            // 添加复选框
                            let mut is_selected = self.selected_articles.contains(&article.id);
                            if ui.checkbox(&mut is_selected, "").clicked() {
                                if is_selected {
                                    self.selected_articles.insert(article.id);
                                } else {
                                    self.selected_articles.remove(&article.id);
                                }
                            }
                            
                            // 发送给AI按钮
                            if ui.small_button("发给AI").clicked() {
                                // 选择当前文章
                                self.selected_articles.clear();
                                self.selected_articles.insert(article.id);
                                // 生成预览内容
                                self.update_ai_send_preview();
                                // 显示AI发送对话框
                                self.show_ai_send_dialog = true;
                            }
                            
                            // 为选中的文章添加更明显的视觉高亮效果
                            let is_selected = self.selected_article_id == Some(article.id);
                            let response = if is_selected {
                                // 选中的文章使用更明显的样式
                                let mut frame = egui::Frame::default();
                                frame = frame.fill(ui.visuals().selection.bg_fill);
                                frame = frame.stroke(egui::Stroke::new(1.0, ui.visuals().selection.stroke.color));
                                frame = frame.inner_margin(egui::Margin::same(1)); // 使用整数而不是浮点数
                                
                                frame.show(ui, |ui| {
                                    ui.selectable_label(true, title)
                                }).inner
                            } else {
                                // 未选中的文章使用默认样式
                                ui.selectable_label(false, title)
                            };
                            
                            if response.clicked() {
                                // 重置翻译相关状态
                                self.is_translating = false;
                                self.translated_article = None;
                                self.show_translated_content = false;
                                
                                self.selected_article_id = Some(article.id);
                                
                                // 自动标记为已读
                                if !article.is_read {
                                    // 克隆必要的参数而不是整个App实例
                                    let storage = self.storage.clone();
                                    let article_id = article.id;
                                    let ui_tx = self.ui_tx.clone();
                                    tokio::spawn(async move {
                                        // 标记文章为已读的逻辑
                                        if let Err(e) = mark_article_as_read_async(storage, article_id, ui_tx).await {
                                            log::error!("Failed to mark article as read: {}", e);
                                        }
                                    });
                                }
                            }
                        });
                        
                        // 优化UI渲染，减少不必要的格式化操作
                        ui.horizontal(|ui| {
                            // 使用配置的时区转换时间
                            let pub_date_text = format!("发布时间: {}", convert_to_configured_timezone(&article.pub_date, &self.config.timezone));
                            ui.label(pub_date_text);
                            
                            if !article.author.is_empty() {
                                ui.separator();
                                let author_text = format!("作者: {}", article.author);
                                ui.label(author_text);
                            }
                        });
                        
                        ui.add_space(8.0);
                    }
                });
            }
            
            // 分页控件 - 放在ScrollArea外部，始终可见
            ui.separator();
            ui.horizontal(|ui| {
                // 显示当前页码和总页数
                ui.label(format!("第 {} 页，共 {} 页，总计 {} 篇文章", 
                                 self.current_page, total_pages, total_articles));
                
                ui.add_space(10.0);
                
                // 首页按钮
                if ui.button("首页").clicked() && self.current_page > 1 {
                    self.current_page = 1;
                }
                
                // 上一页按钮
                if ui.button("上一页").clicked() && self.current_page > 1 {
                    self.current_page -= 1;
                }
                
                // 下一页按钮
                if ui.button("下一页").clicked() && self.current_page < total_pages {
                    self.current_page += 1;
                }
                
                // 末页按钮
                if ui.button("末页").clicked() && self.current_page < total_pages {
                    self.current_page = total_pages;
                }
                
                ui.add_space(10.0);
                
                // 每页显示数量控制
                ui.label("每页显示:");
                ui.add(egui::DragValue::new(&mut self.page_size)
                    .range(10..=100)
                    .speed(10)
                    .clamp_existing_to_range(true));
            });

        });

        // 右侧文章内容面板 - 增大最大宽度以提供更宽敞的阅读体验
        egui::SidePanel::right("article_content")
            .min_width(400.0)
            .max_width(800.0) // 增大最大宽度从600到800
            .frame(egui::Frame::default().fill(sidebar_bg_color))
            .show(ctx, |ui| {
                if self.selected_article_id.is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.label("请选择一篇文章");
                    });
                    return;
                }

                if let Some(article) = self
                    .articles
                    .iter()
                    .find(|a| a.id == self.selected_article_id.unwrap_or(-1))
                {
                    // 文章标题
                    ui.heading(&article.title);

                    // 文章元信息
                    ui.horizontal(|ui| {
                        // 使用配置的时区转换时间
                        ui.label(format!(
                            "发布时间: {}",
                            convert_to_configured_timezone(
                                &article.pub_date,
                                &self.config.timezone
                            )
                        ));
                        ui.separator();
                        ui.label(format!("作者: {}", article.author));
                        ui.separator();
                        ui.label(format!("来源: {}", article.source));
                    });

                    ui.separator();

                    // 文章内容
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add_space(10.0);

                        // 复制需要的状态到闭包外部
                        let is_loading_web_content = self.is_loading_web_content;
                        let current_article_url = self.current_article_url.clone();
                        let show_web_content = self.show_web_content;
                        let web_content = self.web_content.clone();
                        let font_size = self.font_size;
                        let article_link = article.link.clone();

                        // 显示翻译加载状态
                        if self.is_translating {
                            ui.centered_and_justified(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.label("正在翻译文章...");
                            });
                        } 
                        // 显示网页内容加载状态
                        else if is_loading_web_content && current_article_url == article_link {
                            ui.centered_and_justified(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.label("正在加载网页内容...");
                            });
                        } 
                        // 显示网页内容
                        else if show_web_content && current_article_url == article_link {
                            // 渲染加载的网页内容
                            Self::render_article_content_static(
                                ui,
                                &web_content,
                                font_size,
                                &article_link,
                            );
                        } 
                        // 显示翻译后的内容
                        else if self.show_translated_content && self.translated_article.is_some() {
                            if let Some((translated_title, translated_content)) = self.translated_article.as_ref() {
                                // 显示翻译后的标题
                                ui.heading(translated_title);
                                ui.separator();
                                // 渲染翻译后的内容
                                Self::render_article_content_static(
                                    ui,
                                    translated_content,
                                    font_size,
                                    &article.link,
                                );
                            }
                        } 
                        // 显示原始内容
                        else {
                            // 渲染基本的HTML内容
                            Self::render_article_content_static(
                                ui,
                                &article.content,
                                font_size,
                                &article.link,
                            );
                        }

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        // 文章操作按钮 - 移到ScrollArea内部
                        ui.horizontal(|ui| {
                            let is_starred = article.is_starred;
                            if ui
                                .button(format!("{}收藏", if is_starred { "取消" } else { "" }))
                                .clicked()
                            {
                                let storage = self.storage.clone();
                                let ui_tx = self.ui_tx.clone();
                                let article_id = article.id;
                                let new_starred_status = !is_starred;
                                let current_is_read = article.is_read; // 保存当前的已读状态
                                tokio::spawn(async move {
                                    if let Err(e) = toggle_article_starred_async(
                                        storage,
                                        article_id,
                                        new_starred_status,
                                        ui_tx,
                                        current_is_read,
                                    )
                                    .await
                                    {
                                        log::error!(
                                            "Failed to toggle article starred status: {}",
                                            e
                                        );
                                    }
                                });
                            }

                            if ui.button("在浏览器中打开").clicked() {
                                // 使用system_open打开外部链接
                                if let Err(e) = open::that(&article.link) {
                                    log::error!("Failed to open link in browser: {}", e);
                                }
                            }

                            if ui.button("加载网页内容").clicked() {
                                // 使用WebView加载完整网页内容
                                Self::spawn_webview(article.title.clone(), article.link.clone());
                            }

                            if show_web_content && current_article_url == article_link
                                && ui.button("显示原始内容").clicked() {
                                    self.show_web_content = false;
                                }

                            if ui.button("删除文章").clicked() {
                                let storage = self.storage.clone();
                                let search_manager = self.search_manager.clone();
                                let ui_tx_outer = self.ui_tx.clone();
                                let ui_tx_inner = self.ui_tx.clone();
                                let article_id = article.id;
                                tokio::spawn(async move {
                                    if let Err(e) = delete_article_async(
                                        storage,
                                        search_manager,
                                        article_id,
                                        ui_tx_inner,
                                    )
                                    .await
                                    {
                                        log::error!("删除文章失败: {}", e);
                                    } else {
                                        // 删除成功后重新加载文章列表
                                        let _ = ui_tx_outer.send(UiMessage::ReloadArticles);
                                    }
                                });
                            }

                            if ui.button("翻译文章").clicked() {
                                // 只有当AI客户端可用时才执行翻译
                                if let Some(ai_client) = self.ai_client.clone() {
                                    let article_title = article.title.clone();
                                    let article_content = article.content.clone();
                                    let ui_tx = self.ui_tx.clone();
                                    
                                    // 设置翻译状态
                                    self.is_translating = true;
                                    
                                    tokio::spawn(async move {
                                        match ai_client.translate_article(&article_title, &article_content).await {
                                            Ok((translated_title, translated_content)) => {
                                                // 翻译成功，发送结果到UI线程
                                                let _ = ui_tx.send(UiMessage::TranslationCompleted(translated_title, translated_content));
                                            },
                                            Err(e) => {
                                                // 翻译失败，发送错误信息
                                                log::error!("翻译文章失败: {:?}", e);
                                                let _ = ui_tx.send(UiMessage::Error(format!("翻译文章失败: {:?}", e)));
                                                // 发送重置翻译状态的消息
                                                let _ = ui_tx.send(UiMessage::RequestRepaint);
                                            }
                                        }
                                    });
                                }
                            }

                            // 翻译切换按钮
                            if self.translated_article.is_some()
                                && ui.button(if self.show_translated_content { "显示原文" } else { "显示翻译" }).clicked() {
                                    self.show_translated_content = !self.show_translated_content;
                                }
                        });

                        // 添加额外的底部空间，确保按钮不被紧贴底部
                        ui.add_space(20.0);
                    });
                }
            });

        // 添加订阅源对话框
        if self.show_add_feed_dialog {
            egui::Window::new("添加订阅源")
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("标题");
                    ui.text_edit_singleline(&mut self.new_feed_title);

                    ui.label("URL");
                    ui.text_edit_singleline(&mut self.new_feed_url);

                    ui.label("分组（可选）");
                    ui.text_edit_singleline(&mut self.new_feed_group);

                    ui.checkbox(&mut self.new_feed_auto_update, "自动更新");
                    ui.checkbox(&mut self.new_feed_ai_auto_translate, "AI自动翻译");

                    ui.horizontal(|ui| {
                        if ui.button("添加").clicked() {
                            let feed_url = self.new_feed_url.clone();
                            let feed_title = self.new_feed_title.clone();
                            let feed_group = self.new_feed_group.clone();
                            let auto_update = self.new_feed_auto_update;
                            let ai_auto_translate = self.new_feed_ai_auto_translate;
                            let storage = self.storage.clone();
                            let rss_fetcher = self.rss_fetcher.clone();
                            let feed_manager = self.feed_manager.clone();
                            let ui_tx = self.ui_tx.clone();
                            tokio::spawn(async move {
                                if let Err(e) = add_feed_async(
                                    feed_url,
                                    feed_title,
                                    feed_group,
                                    auto_update,
                                    ai_auto_translate,
                                    storage,
                                    rss_fetcher,
                                    feed_manager,
                                    ui_tx,
                                )
                                .await
                                {
                                    log::error!("Failed to add feed: {}", e);
                                } else {
                                    log::info!("Feed added successfully");
                                }
                            });
                            self.show_add_feed_dialog = false;
                        }

                        if ui.button("取消").clicked() {
                            self.edit_feed_auto_update = true; // 重置为默认值
                            self.new_feed_url.clear();
                            self.new_feed_title.clear();
                            self.new_feed_group.clear();
                            self.new_feed_auto_update = true; // 重置为默认值
                            self.new_feed_ai_auto_translate = false; // 重置为默认值
                            self.show_add_feed_dialog = false;
                        }
                    });
                });
        }

        // 编辑订阅源对话框
        if self.show_edit_feed_dialog {
            egui::Window::new("编辑订阅源")
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("标题");
                    ui.text_edit_singleline(&mut self.edit_feed_title);

                    ui.label("URL");
                    ui.text_edit_singleline(&mut self.edit_feed_url);

                    ui.label("分组（可选）");
                    ui.text_edit_singleline(&mut self.edit_feed_group);

                    ui.checkbox(&mut self.edit_feed_auto_update, "自动更新");

                    ui.checkbox(&mut self.edit_feed_enable_notification, "启用通知");

                    ui.checkbox(&mut self.edit_feed_ai_auto_translate, "AI自动翻译");

                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            if let Some(feed_id) = self.edit_feed_id {
                                let feed_title = self.edit_feed_title.clone();
                                let feed_url = self.edit_feed_url.clone();
                                let feed_group = self.edit_feed_group.clone();
                                let storage = self.storage.clone();
                                let ui_tx = self.ui_tx.clone();

                                let feed_auto_update = self.edit_feed_auto_update;
                                let feed_enable_notification = self.edit_feed_enable_notification;
                                let feed_ai_auto_translate = self.edit_feed_ai_auto_translate;
                                // 异步更新订阅源
                                tokio::spawn(async move {
                                    update_feed_async(
                                        storage,
                                        ui_tx,
                                        feed_id,
                                        feed_title,
                                        feed_url,
                                        feed_group,
                                        feed_auto_update,
                                        feed_enable_notification,
                                        feed_ai_auto_translate,
                                    )
                                    .await;
                                });
                            }

                            // 重置编辑状态
                            self.edit_feed_id = None;
                            self.edit_feed_url.clear();
                            self.edit_feed_title.clear();
                            self.edit_feed_group.clear();
                            self.show_edit_feed_dialog = false;
                        }

                        if ui.button("取消").clicked() {
                            self.edit_feed_id = None;
                            self.edit_feed_url.clear();
                            self.edit_feed_title.clear();
                            self.edit_feed_group.clear();
                            self.show_edit_feed_dialog = false;
                        }
                    });
                });
        }

        // 编辑分组对话框
        if self.show_edit_group_dialog {
            egui::Window::new("编辑分组")
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("当前分组名称: {}", self.editing_group_name));

                    ui.label("新分组名称");
                    ui.text_edit_singleline(&mut self.new_group_name);

                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            let old_name = self.editing_group_name.clone();
                            let new_name = self.new_group_name.clone();
                            let feed_manager = self.feed_manager.clone();
                            let ui_tx = self.ui_tx.clone();

                            // 异步更新分组名称
                            tokio::spawn(async move {
                                log::debug!("开始更新分组名称，旧名称: '{}', 新名称: '{}'", old_name, new_name);
                                
                                // 只获取一次feed_manager锁
                                let feed_manager_lock = feed_manager.lock().await;
                                
                                // 通过名称获取分组ID
                                if let Ok(Some(group_id)) = feed_manager_lock.get_group_id_by_name(&old_name).await {
                                    log::debug!("成功获取分组ID: {}，对应旧名称: '{}'", group_id, old_name);
                                    
                                    // 调用rename_group方法，使用分组ID
                                    log::debug!("开始调用rename_group方法，分组ID: {}, 新名称: '{}'", group_id, new_name);
                                    if let Err(e) = feed_manager_lock.rename_group(group_id, &new_name).await {
                                        log::error!("调用rename_group失败: {}，分组ID: {}, 新名称: '{}'", e, group_id, new_name);
                                    } else {
                                        log::debug!("成功更新分组名称，分组ID: {}, 旧名称: '{}', 新名称: '{}'", group_id, old_name, new_name);
                                        // 发送消息通知UI更新
                                        if let Err(e) = ui_tx.send(UiMessage::AllFeedsRefreshed) {
                                            log::error!("发送AllFeedsRefreshed消息失败: {}", e);
                                        } else {
                                            log::debug!("成功发送AllFeedsRefreshed消息");
                                        }
                                    }
                                } else {
                                    log::error!("获取分组ID失败，旧名称: '{}'", old_name);
                                }
                            });

                            // 重置编辑状态
                            self.show_edit_group_dialog = false;
                            self.editing_group_name.clear();
                            self.new_group_name.clear();


                            // 清除分组缓存，强制重新计算分组
                        }

                        if ui.button("取消").clicked() {
                            self.show_edit_group_dialog = false;

                            self.editing_group_name.clear();
                            self.new_group_name.clear();
                        }
                    });
                });
        }

        // AI发送选项对话框
        if self.show_ai_send_dialog {
            egui::Window::new("发送到AI")
                .resizable(true)
                .max_width(600.0)
                .max_height(500.0)
                .show(ctx, |ui| {
                    ui.label("选择发送内容类型:");
                    
                    ui.horizontal(|ui| {
                        if ui.radio_value(&mut self.ai_send_content_type, AISendContentType::TitleOnly, "仅标题").clicked() {
                            // 更新预览
                            self.update_ai_send_preview();
                        }
                        if ui.radio_value(&mut self.ai_send_content_type, AISendContentType::ContentOnly, "仅内容").clicked() {
                            // 更新预览
                            self.update_ai_send_preview();
                        }
                        if ui.radio_value(&mut self.ai_send_content_type, AISendContentType::Both, "标题和内容").clicked() {
                            // 更新预览
                            self.update_ai_send_preview();
                        }
                    });
                    
                    ui.separator();
                    
                    ui.label("发送内容预览:");
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.text_edit_multiline(&mut self.ai_send_preview);
                    });
                    
                    ui.separator();
                    
                    ui.horizontal(|ui| {
                        if ui.button("发送").clicked() {
                            // 显示AI对话窗口
                            self.show_ai_chat_window = true;
                            
                            // 将预览内容传递到AI对话输入框
                           // self.ai_chat_input = self.ai_send_preview.clone();
                            
                            // 创建或获取AI聊天会话
                            let mut _session_created = false;
                            if self.ai_chat_session.is_none() {
                                if let Some(client) = &self.ai_client {
                                    let mut session = AIChatSession::new(client.clone());
                                    // 设置系统提示
                                    session.set_system_prompt("你是一个有用的助手。请根据提供的文章内容，给出详细、准确的回答。");
                                    // 加载聊天历史记录
                                    if let Err(e) = session.load_chat_history() {
                                        log::error!("加载聊天历史记录失败: {}", e);
                                    }
                                    self.ai_chat_session = Some(session);
                                    _session_created = true;
                                }
                            } else {
                                // 加载聊天历史记录
                                if let Some(session) = self.ai_chat_session.as_mut()
                                    && let Err(e) = session.load_chat_history() {
                                        log::error!("加载聊天历史记录失败: {}", e);
                                    }
                            }
                            
                            // 发送内容到AI
                            let content = self.ai_send_preview.clone();
                            let ui_tx = self.ui_tx.clone();
                            
                            // 克隆会话以便在异步任务中使用
                            if let Some(mut session) = self.ai_chat_session.clone() {
                                // 创建流式响应回调
                                let (stream_tx, stream_rx) = std::sync::mpsc::channel();
                                
                                // 使用异步任务发送请求，避免阻塞UI
                                tokio::spawn(async move {
                                    // 标记开始流式响应
                                    let _ = stream_tx.send(Some("stream_start".to_string()));
                                    
                                    let result = session.send_message(&content, Some(&mut |content| {
                                        // 发送流式响应内容
                                        let _ = stream_tx.send(Some(content.to_string()));
                                        Ok(())
                                    })).await;
                                    
                                    match result {
                                        Ok(_) => {
                                            // 发送成功，标记结束
                                            let _ = stream_tx.send(Some("stream_end".to_string()));
                                            let _ = ui_tx.send(UiMessage::StatusMessage("发送成功".to_string()));
                                            // 保存聊天历史记录
                                            if let Err(e) = session.save_chat_history() {
                                                log::error!("保存聊天历史记录失败: {}", e);
                                            }
                                        },
                                        Err(e) => {
                                            // 发送失败，显示错误信息
                                            log::error!("AI消息发送失败: {:?}", e);
                                            let _ = stream_tx.send(None);
                                            let _ = ui_tx.send(UiMessage::Error(format!("发送失败: {}", e)));
                                        }
                                    }
                                });
                                
                                // 处理流式响应
                                self.is_sending_message = true;
                                self.is_ai_streaming = true;
                                self.current_ai_stream_response.clear();
                                
                                // 在UI线程中处理流式响应
                                let ui_tx_clone = self.ui_tx.clone();
                                std::thread::spawn(move || {
                                    while let Ok(stream_data) = stream_rx.recv() {
                                        match stream_data {
                                            Some(data) => {
                                                if data == "stream_start" {
                                                    // 开始流式响应
                                                    let _ = ui_tx_clone.send(UiMessage::StatusMessage("正在接收AI响应...".to_string()));
                                                } else if data == "stream_end" {
                                                    // 结束流式响应
                                                    let _ = ui_tx_clone.send(UiMessage::StatusMessage("AI响应完成".to_string()));
                                                    let _ = ui_tx_clone.send(UiMessage::AIStreamEnd);
                                                    
                                                    break;
                                                } else {
                                                    // 接收流式响应内容
                                                    let _ = ui_tx_clone.send(UiMessage::AIStreamData(data));
                                                }
                                            }
                                            None => {
                                                // 发生错误
                                                let _ = ui_tx_clone.send(UiMessage::StatusMessage("AI响应出错".to_string()));
                                                let _ = ui_tx_clone.send(UiMessage::AIStreamEnd);
                                                break;
                                            }
                                        }
                                    }
                                });
                            }
                            
                            self.status_message = "正在发送到AI...".to_string();
                            self.show_ai_send_dialog = false;
                        }
                        
                        if ui.button("取消").clicked() {
                            self.show_ai_send_dialog = false;
                        }
                    });
                });
        }

        // AI设置对话框
        if self.show_ai_settings_dialog {
            // 创建临时变量来存储输入值
            egui::Window::new("AI设置")
                .resizable(true)
                .default_width(500.0)
                .show(ctx, |ui| {
                    ui.label("API URL:");
                    ui.text_edit_singleline(&mut self.ai_settings_api_url);

                    ui.label("API Key:");
                    ui.text_edit_singleline(&mut self.ai_settings_api_key);

                    ui.label("模型名称:");
                    ui.text_edit_singleline(&mut self.ai_settings_model_name);

                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            // 更新配置
                            self.config.ai_api_url = self.ai_settings_api_url.clone();
                            self.config.ai_api_key = self.ai_settings_api_key.clone();
                            self.config.ai_model_name = self.ai_settings_model_name.clone();

                            // 保存配置到文件
                            if let Err(e) = self.config.save() {
                                log::error!("保存AI设置失败: {}", e);
                                self.status_message = format!("保存AI设置失败: {}", e);
                            } else {
                                // 重新初始化AI客户端
                                self.ai_client = AIClient::new(
                                    &self.ai_settings_api_url,
                                    &self.ai_settings_api_key,
                                    &self.ai_settings_model_name,
                                )
                                .ok();
                                self.status_message = "AI设置已保存".to_string();
                            }

                            self.show_ai_settings_dialog = false;
                        }

                        if ui.button("取消").clicked() {
                            self.show_ai_settings_dialog = false;
                        }
                    });
                });
        }

        // AI对话窗口
        if self.show_ai_chat_window {
            // 确保AI聊天会话存在
            if self.ai_chat_session.is_none() {
                if let Some(client) = &self.ai_client {
                    let mut session = AIChatSession::new(client.clone());
                    // 设置系统提示
                    session.set_system_prompt(
                        "你是一个有用的助手。请根据提供的文章内容，给出详细、准确的回答。",
                    );
                    // 加载聊天历史记录
                    if let Err(e) = session.load_chat_history() {
                        log::error!("加载聊天历史记录失败: {}", e);
                    }
                    self.ai_chat_session = Some(session);
                }
            } else {
                // 加载聊天历史记录
                if let Some(session) = self.ai_chat_session.as_mut()
                    && let Err(e) = session.load_chat_history() {
                        log::error!("加载聊天历史记录失败: {}", e);
                    }
            }

            egui::Window::new("AI对话")
                .resizable(true)
                .default_width(800.0)
                .default_height(600.0)
                .show(ctx, |ui| {
                    // 对话历史区域标题（可选）
                    ui.label("对话历史:");

                    // 计算可用高度
                    let total_height = ui.available_height();

                    // 固定输入 + 按钮区域总高度
                    const INPUT_AND_BUTTON_HEIGHT: f32 = 150.0;

                    // 对话历史区域高度
                    let chat_history_height = (total_height - INPUT_AND_BUTTON_HEIGHT).max(100.0);

                    // 对话历史 — 放在 ScrollArea 中
                    egui::ScrollArea::vertical()
                        .max_height(chat_history_height)
                        .stick_to_bottom(true) // 新消息时自动滚到底部（可选）
                        .show(ui, |ui| {
                            if let Some(session) = &self.ai_chat_session {
                                for message in session.get_messages() {
                                    ui.group(|ui| {
                                        ui.with_layout(
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{}:",
                                                        message.role
                                                    ))
                                                    .strong(),
                                                );
                                                ui.add(egui::Label::new(&message.content).wrap());
                                            },
                                        );
                                    });
                                    ui.add_space(8.0);
                                }
                            } else {
                                ui.label("暂无对话历史");
                            }
                            // 若在流式响应中，也显示当前内容
                            if self.is_ai_streaming {
                                ui.group(|ui| {
                                    ui.with_layout(
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.label(egui::RichText::new("assistant:").strong());
                                            ui.add(
                                                egui::Label::new(&self.current_ai_stream_response)
                                                    .wrap(),
                                            );
                                        },
                                    );
                                });
                                ui.add_space(8.0);
                            }
                        });

                    ui.separator();

                    // — 输入 + 发送按钮 区域 —
                    // 用 vertical 布局分为：固定高度输入框 + 按钮行
                    ui.vertical(|ui| {
                        // 输入框：用 ScrollArea + TextEdit，固定高度 80px（或你需要的高度）
                        egui::ScrollArea::vertical()
                            .max_height(80.0)
                            .auto_shrink([false, false]) // 禁用自动收缩，确保占满可用宽度
                            .show(ui, |ui| {
                                // 使用ui.add_sized确保TextEdit占满整个宽度
                                ui.add_sized(
                                    [ui.available_width(), 80.0],
                                    egui::TextEdit::multiline(&mut self.ai_chat_input)
                                        .code_editor()
                                        .desired_rows(3), // 你可以考虑 .frame(true) / .margin(...) 调整样式
                                );
                            });

                        // 按钮行
                        ui.horizontal(|ui| {
                            let send_button_enabled =
                                !self.ai_chat_input.trim().is_empty() && !self.is_sending_message;
                            if ui
                                .add_enabled(send_button_enabled, egui::Button::new("发送"))
                                .clicked()
                            {
                                // … 你的发送逻辑 …
                                let message = self.ai_chat_input.clone();
                                let ui_tx = self.ui_tx.clone(); // 克隆会话以便在异步任务中使用 
                                if let Some(mut session) = self.ai_chat_session.clone() {
                                    // 创建流式响应回调
                                    let (stream_tx, stream_rx) = std::sync::mpsc::channel();
                                    // 使用异步任务发送请求，避免阻塞UI
                                    tokio::spawn(async move {
                                        // 标记开始流式响应
                                        let _ = stream_tx.send(Some("stream_start".to_string()));
                                        let result = session
                                            .send_message(
                                                &message,
                                                Some(&mut |content| {
                                                    // 发送流式响应内容
                                                    let _ =
                                                        stream_tx.send(Some(content.to_string()));
                                                    Ok(())
                                                }),
                                            )
                                            .await;
                                        match result {
                                            Ok(_) => {
                                                // 发送成功，标记结束
                                                let _ =
                                                    stream_tx.send(Some("stream_end".to_string()));
                                                let _ = ui_tx.send(UiMessage::StatusMessage(
                                                    "发送成功".to_string(),
                                                ));
                                            }
                                            Err(e) => {
                                                // 发送失败，显示错误信息
                                                log::error!("AI消息发送失败: {:?}", e);
                                                let _ = stream_tx.send(None);
                                                let _ = ui_tx.send(UiMessage::Error(format!(
                                                    "发送失败: {}",
                                                    e
                                                )));
                                            }
                                        }
                                    });
                                    // 处理流式响应
                                    self.is_sending_message = true;
                                    self.is_ai_streaming = true;
                                    self.current_ai_stream_response.clear();
                                    // 在UI线程中处理流式响应
                                    let ui_tx_clone = self.ui_tx.clone();
                                    std::thread::spawn(move || {
                                        while let Ok(stream_data) = stream_rx.recv() {
                                            match stream_data {
                                                Some(data) => {
                                                    if data == "stream_start" {
                                                        // 开始流式响应
                                                        let _ = ui_tx_clone.send(
                                                            UiMessage::StatusMessage(
                                                                "正在接收AI响应...".to_string(),
                                                            ),
                                                        );
                                                    } else if data == "stream_end" {
                                                        // 结束流式响应
                                                        let _ = ui_tx_clone.send(
                                                            UiMessage::StatusMessage(
                                                                "AI响应完成".to_string(),
                                                            ),
                                                        );
                                                        let _ = ui_tx_clone
                                                            .send(UiMessage::AIStreamEnd);
                                                        break;
                                                    } else {
                                                        // 接收流式响应内容
                                                        let _ = ui_tx_clone
                                                            .send(UiMessage::AIStreamData(data));
                                                    }
                                                }
                                                None => {
                                                    // 发生错误
                                                    let _ =
                                                        ui_tx_clone.send(UiMessage::StatusMessage(
                                                            "AI响应出错".to_string(),
                                                        ));
                                                    let _ =
                                                        ui_tx_clone.send(UiMessage::AIStreamEnd);
                                                    break;
                                                }
                                            }
                                        }
                                    });
                                    self.status_message = "正在发送到AI...".to_string();
                                    self.ai_chat_input.clear();
                                }
                            }
                            if ui.button("清除历史").clicked()
                                && let Some(session) = &mut self.ai_chat_session {
                                    session.clear_history();
                                    self.status_message = "已清除对话历史".to_string();
                                    // 删除聊天历史记录文件
                                    if let Err(e) = AIChatSession::delete_chat_history() {
                                        log::error!("删除聊天历史记录文件失败: {}", e);
                                    }
                                }
                            if ui.button("关闭").clicked() {
                                self.show_ai_chat_window = false;
                            }
                        });
                    });
                });
        }

        // 状态栏
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // 显示状态信息，限制宽度并允许换行
                if !self.status_message.is_empty() {
                    // 使用固定宽度的标签显示状态信息，避免破坏UI布局
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(&self.status_message).wrap()); // 允许文本换行
                    });
                    ui.separator();
                }

                // 显示统计信息
                ui.label(format!(
                    "订阅源: {}, 文章: {}",
                    self.feeds.len(),
                    self.articles.len()
                ));

                // 显示自动更新倒计时
                if self.auto_update_enabled {
                    ui.separator();
                    ui.label(format!("下次更新: {}秒", self.auto_update_countdown));
                }

                // 显示刷新状态
                if self.is_refreshing {
                    ui.separator();
                    ui.add(egui::Spinner::new());
                }
            });
        });
    }
}

/// 刷新单个订阅源的异步辅助函数
async fn refresh_single_feed_async(
    storage: Arc<Mutex<StorageManager>>,
    rss_fetcher: Arc<Mutex<RssFetcher>>,
    feed_id: i64,
    notification_manager: Option<Arc<Mutex<NotificationManager>>>,
    ui_tx: Option<Sender<UiMessage>>,
    ai_client: Option<Arc<crate::ai_client::AIClient>>,
) -> anyhow::Result<()> {
    // 获取订阅源信息
    let feed = {
        let storage = storage.lock().await;
        let feeds = storage.get_all_feeds().await?;
        feeds
            .into_iter()
            .find(|f| f.id == feed_id)
            .ok_or_else(|| anyhow::anyhow!("Feed not found: {}", feed_id))?
    };

    // 获取更新前的文章
    let old_articles = {
        let storage_lock = storage.lock().await;
        storage_lock.get_articles_by_feed(feed.id).await?
    };
    let old_article_count = old_articles.len();

    // 调用update_feed_internal函数，复用AI翻译和重复文章检查逻辑
    update_feed_internal(feed_id, &feed.url, storage.clone(), rss_fetcher.clone(), ai_client).await?;

    // 获取更新后的文章数量和列表
    let (current_article_count, new_articles) = {
        let storage_lock = storage.lock().await;
        let new_articles = storage_lock.get_articles_by_feed(feed.id).await?;
        (new_articles.len(), new_articles)
    };

    // 检查是否有新文章
    if current_article_count > old_article_count {
        // 获取真正的新文章
        let added_articles = article_processor::get_new_articles(&old_articles, &new_articles);
        let new_count = added_articles.len();
        
        if new_count > 0 {
            log::info!("订阅源 {} 有 {} 篇新文章", feed.title, new_count);

            // 按发布时间倒序排列，取最新的文章
            let mut sorted_articles = added_articles; // 直接使用，无需clone
            sorted_articles.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));

            // 将所有新文章标题添加到通知列表
            let mut new_article_titles = Vec::new();
            for article in sorted_articles.iter() {
                new_article_titles.push(article.title.clone());
            }

            // 发送新文章通知
            article_processor::send_new_articles_notification(
                notification_manager,
                &feed.title,
                &new_article_titles,
                feed.enable_notification,
            );
        }
    }

    // 发送UI更新消息
    if let Some(ui_tx) = ui_tx {
        // 发送ArticlesLoaded消息，更新当前订阅源的文章列表
        let _ = ui_tx.send(UiMessage::ArticlesLoaded(new_articles));
    }

    Ok(())
}

/// 标记文章为已读的异步辅助函数
async fn mark_article_as_read_async(
    storage: Arc<Mutex<StorageManager>>,
    article_id: i64,
    ui_tx: Sender<UiMessage>,
) -> anyhow::Result<()> {
    let mut storage = storage.lock().await;
    storage.update_article_read_status(article_id, true).await?;

    // 发送消息更新UI状态
    ui_tx.send(UiMessage::ArticleStatusUpdated(article_id, true, false))?;

    Ok(())
}

/// 标记所有文章为已读的异步辅助函数
async fn mark_all_as_read_async(
    storage: Arc<Mutex<StorageManager>>,
    feed_id: Option<i64>,
    ui_tx: Sender<UiMessage>,
) -> anyhow::Result<()> {
    let mut storage = storage.lock().await;
    storage.mark_all_as_read(feed_id).await?;

    // 发送消息更新UI状态
    let status_message = if let Some(_id) = feed_id {
        "已将订阅源的所有文章标记为已读".to_string()
    } else {
        "已将所有文章标记为已读".to_string()
    };

    if let Err(e) = ui_tx.send(UiMessage::StatusMessage(status_message)) {
        log::error!("发送状态消息失败: {}", e);
    }

    Ok(())
}

/// 删除文章的异步辅助函数
async fn delete_article_async(
    storage: Arc<Mutex<StorageManager>>,
    search_manager: Arc<Mutex<SearchManager>>,
    article_id: i64,
    ui_tx: Sender<UiMessage>,
) -> anyhow::Result<()> {
    let mut storage = storage.lock().await;

    // 获取文章信息，以便获取 feed_id
    let article = storage.get_article(article_id).await?;
    let feed_id = article.feed_id;

    // 删除文章
    storage.delete_article(article_id).await?;
    drop(storage);
    // 加载配置，判断是否需要更新搜索索引
    let config = crate::config::AppConfig::load_or_default();
    if config.search_mode == "index_search" {
        // 从搜索索引中移除文章
        let search_manager = search_manager.lock().await;
        search_manager.remove_article(article_id, feed_id).await;
    }

    // 发送状态消息
    if let Err(e) = ui_tx.send(UiMessage::StatusMessage("文章已删除".to_string())) {
        log::error!("发送状态消息失败: {}", e);
    }

    Ok(())
}

async fn delete_all_articles_async(
    storage: Arc<Mutex<StorageManager>>,
    search_manager: Arc<Mutex<SearchManager>>,
    feed_id: Option<i64>,
    ui_tx: Sender<UiMessage>,
) -> anyhow::Result<()> {
    let mut storage = storage.lock().await;
    storage.delete_all_articles(feed_id).await?;
     drop(storage);
    // 加载配置，判断是否需要更新搜索索引
    let config = crate::config::AppConfig::load_or_default();
    if config.search_mode == "index_search" {
        // 更新搜索索引
        let search_manager = search_manager.lock().await;
        match feed_id {
            Some(id) => {
                // 只删除特定订阅源的文章索引
                search_manager.remove_articles_by_feed(id).await;
            }
            None => {
                // 清空所有搜索索引
                search_manager.clear().await;
            }
        }
    }

    // 发送状态消息
    if let Err(e) = ui_tx.send(UiMessage::StatusMessage("文章已批量删除".to_string())) {
        log::error!("发送状态消息失败: {}", e);
    }

    Ok(())
}

/// 异步获取特定订阅源的文章并更新UI
async fn get_articles_by_feed_async(
    storage: Arc<Mutex<StorageManager>>,
    feed_id: i64,
    ui_tx: Sender<UiMessage>,
) -> anyhow::Result<()> {
    let storage = storage.lock().await;
    match storage.get_articles_by_feed(feed_id).await {
        Ok(articles) => {
            // 通过消息通道将文章数据发送到UI线程
            if let Err(e) = ui_tx.send(UiMessage::ArticlesLoaded(articles)) {
                log::error!("发送文章加载消息失败: {}", e);
            }
            Ok(())
        }
        Err(e) => {
            log::error!("加载文章失败: {}", e);
            // 发送错误消息到UI线程
            if let Err(ui_error) = ui_tx.send(UiMessage::Error(format!("加载文章失败: {}", e)))
            {
                log::error!("发送错误消息失败: {}", ui_error);
            }
            Err(e)
        }
    }
}

/// 获取所有文章的辅助函数
/// 异步更新订阅源信息的辅助函数
#[allow(clippy::too_many_arguments)]
async fn update_feed_async(
    storage: Arc<Mutex<StorageManager>>,
    ui_tx: Sender<UiMessage>,
    feed_id: i64,
    title: String,
    url: String,
    group: String,
    auto_update: bool,         // 自动更新设置
    enable_notification: bool, // 通知设置
    ai_auto_translate: bool,   // AI自动翻译设置
) {
    // 验证输入
    if title.is_empty() || url.is_empty() {
        let _ = ui_tx.send(UiMessage::Error("标题和URL不能为空".to_string()));
        return;
    }

    // 获取现有的订阅源信息
    let mut feed = match storage.lock().await.get_all_feeds().await {
        Ok(feeds) => feeds
            .into_iter()
            .find(|f| f.id == feed_id)
            .ok_or_else(|| anyhow::anyhow!("订阅源不存在: {}", feed_id)),
        Err(e) => Err(e),
    };

    if let Ok(feed) = &mut feed {
        // 更新订阅源信息
        feed.title = title;
        feed.url = url;
        feed.group = group;
        feed.auto_update = auto_update; // 更新自动更新设置
        feed.enable_notification = enable_notification; // 更新通知设置
        feed.ai_auto_translate = ai_auto_translate; // 更新AI自动翻译设置

        // 保存到数据库
        if let Err(e) = storage.lock().await.update_feed(feed).await {
            log::error!("更新订阅源失败: {}", e);
            let _ = ui_tx.send(UiMessage::Error(format!("更新订阅源失败: {}", e)));
            return;
        }

        // 重新加载所有订阅源以更新UI
        if let Ok(feeds) = storage.lock().await.get_all_feeds().await {
            let _ = ui_tx.send(UiMessage::FeedsLoaded(feeds));
            let _ = ui_tx.send(UiMessage::StatusMessage(format!(
                "成功更新订阅源: {}",
                feed.title
            )));
        }
    } else if let Err(e) = feed {
        log::error!("更新订阅源失败: {}", e);
        let _ = ui_tx.send(UiMessage::Error(format!("更新订阅源失败: {}", e)));
    }
}

// 实现Drop trait，确保应用退出时正确清理资源
impl Drop for App {
    fn drop(&mut self) {
        log::info!("App实例正在被销毁，清理资源...");

        // 停止自动更新任务
        self.stop_auto_update();

        // 关闭UI消息发送器（会导致接收端的recv返回错误，自然退出循环）
        drop(self.ui_tx.clone());

        // 释放系统托盘资源（通过TrayManager的Drop实现自动清理）
        if self.tray_manager.take().is_some() {
            log::debug!("释放系统托盘管理器");
        }

        // 释放系统托盘消息接收器
        if self.tray_receiver.take().is_some() {
            log::debug!("释放系统托盘消息接收器");
        }

        log::info!("App资源清理完成");
    }
}
