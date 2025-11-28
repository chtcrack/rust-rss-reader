// 通知管理模块

use crate::app::UiMessage;
use crate::models::Article;
use log::{error, info, warn};
use notify_rust::{Notification, Timeout};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

/// 通知事件类型
enum NotificationEvent {
    /// 发送新文章通知
    NewArticles(Vec<(String, Article)>),
    /// 更新通知配置
    UpdateConfig(bool, usize, u64),
    /// 更新UI消息发送器
    UpdateUiSender(Sender<UiMessage>),
    /// 停止通知服务
    Stop,
}

/// 通知管理器配置
#[derive(Clone, Debug)]
struct NotificationConfig {
    /// 是否启用通知
    enabled: bool,
    /// 最大通知数量
    max_notifications: usize,
    /// 通知超时时间（毫秒）
    timeout_ms: u64,
}

/// 通知管理器
pub struct NotificationManager {
    /// 通知事件发送器
    event_sender: Sender<NotificationEvent>,
    /// 配置
    #[allow(unused)]
    config: Arc<Mutex<NotificationConfig>>,
    /// UI消息发送器，用于通知UI选择文章
    ui_sender: Option<Sender<UiMessage>>,
}

impl NotificationManager {
    /// 使用配置创建新的通知管理器
    pub fn new(config: crate::config::AppConfig) -> Self {
        // 创建事件通道
        let (event_sender, event_receiver) = channel();

        // 创建配置
        let config = Arc::new(Mutex::new(NotificationConfig {
            enabled: config.enable_notifications,
            max_notifications: config.max_notifications,
            timeout_ms: config.notification_timeout_ms,
        }));

        // 启动通知处理线程
        let config_clone = Arc::clone(&config);
        thread::spawn(move || {
            Self::notification_thread(event_receiver, config_clone, None);
        });

        Self {
            event_sender,
            config,
            ui_sender: None,
        }
    }

    /// 设置UI消息发送器
    pub fn set_ui_sender(&mut self, ui_sender: Sender<UiMessage>) {
        self.ui_sender = Some(ui_sender.clone());

        // 更新通知处理线程的UI发送器
        if let Err(e) = self
            .event_sender
            .send(NotificationEvent::UpdateUiSender(ui_sender))
        {
            error!("更新UI发送器失败: {:?}", e);
        }
    }

    /// 通知处理线程
    fn notification_thread(
        receiver: Receiver<NotificationEvent>,
        config: Arc<Mutex<NotificationConfig>>,
        mut ui_sender: Option<Sender<UiMessage>>,
    ) {
        // 通知队列，用于控制短时间内的通知数量
        let mut notification_queue = VecDeque::new();
        const QUEUE_CLEAN_INTERVAL: Duration = Duration::from_secs(5);
        let mut last_clean_time = Instant::now();

        loop {
            // 检查是否需要清理过期的通知记录
            if Instant::now().duration_since(last_clean_time) > QUEUE_CLEAN_INTERVAL {
                Self::clean_notification_queue(&mut notification_queue);
                last_clean_time = Instant::now();
            }

            // 接收通知事件，设置超时以便定期清理队列
            match receiver.recv_timeout(QUEUE_CLEAN_INTERVAL) {
                Ok(event) => match event {
                    NotificationEvent::NewArticles(feed_articles) => {
                        let config = config.lock().unwrap();
                        if !config.enabled {
                            continue; // 通知已禁用
                        }

                        // 限制通知数量
                        let articles_to_notify = &feed_articles
                            [0..std::cmp::min(feed_articles.len(), config.max_notifications)];

                        // 发送通知
                        for (feed_title, article) in articles_to_notify {
                            Self::send_notification(
                                &format!(
                                    "[{feed_title}] {}",
                                    Self::truncate_text(&article.title, 50)
                                ),
                                &Self::truncate_text(&article.summary, 200),
                                config.timeout_ms,
                                article.clone(),
                                ui_sender.clone(),
                            );

                            // 将通知添加到队列，用于速率限制
                            notification_queue.push_back(Instant::now());

                            // 限制通知频率，避免通知风暴
                            thread::sleep(Duration::from_millis(100));
                        }
                    }

                    NotificationEvent::UpdateConfig(enabled, max_notifications, timeout_ms) => {
                        let mut config = config.lock().unwrap();
                        config.enabled = enabled;
                        config.max_notifications = max_notifications;
                        config.timeout_ms = timeout_ms;
                        info!(
                            "通知配置已更新: enabled={}, max_notifications={}, timeout_ms={}",
                            config.enabled, config.max_notifications, config.timeout_ms
                        );
                    }

                    NotificationEvent::UpdateUiSender(new_ui_sender) => {
                        ui_sender = Some(new_ui_sender);
                        info!("UI发送器已更新");
                    }

                    NotificationEvent::Stop => {
                        info!("通知服务已停止");
                        break;
                    }
                },
                Err(_) => {
                    // 超时，继续循环以清理队列
                    continue;
                }
            }
        }
    }

    /// 清理过期的通知记录（1分钟前的）
    fn clean_notification_queue(queue: &mut VecDeque<Instant>) {
        let cutoff_time = Instant::now() - Duration::from_secs(60);

        // 移除所有过期的时间戳
        while queue.front().map_or(false, |&time| time < cutoff_time) {
            queue.pop_front();
        }
    }

    /// 发送单个通知
    fn send_notification(
        summary: &str,
        body: &str,
        timeout_ms: u64,
        article: Article,
        _ui_sender: Option<Sender<UiMessage>>,
    ) {
        // 使用notify-rust发送通知
        // 在Windows上，我们需要确保使用正确的通知方式
        info!("准备发送通知: {}", summary);

        // 将文章ID编码到动作标识符中
        let view_action = format!("view_{}", article.id);

        // 创建通知实例并配置
        let result = Notification::new()
            .appname("RustRSSReader.UniqueAppID")
            .summary(summary)
            .body(body)
            // 添加查看按钮，动作标识符包含文章ID
            .action(&view_action, "查看")
            // 添加忽略按钮
            .action("dismiss", "忽略")
            // 将u64转换为u32，添加安全检查
            .timeout(Timeout::Milliseconds(timeout_ms.try_into().unwrap_or(5000)))
            .show();

        match result {
            Ok(_notification_handle) => {
                info!("通知已成功发送: {}", summary);
                #[cfg(target_os = "windows")]
                info!(
                    "Windows通知已使用应用ID 'RustRSSReader.UniqueAppID' 发送，请在系统设置中查找此应用名称"
                );

                // 在新线程中处理通知交互，避免阻塞
                std::thread::spawn(move || {
                    // 只有在非Windows平台上才支持wait_for_action
                    #[cfg(not(target_os = "windows"))]
                    {
                        // 克隆必要的数据以在非Windows平台上使用
                        let article_id = article.id;
                        let ui_sender_clone = _ui_sender.clone();

                        // 在非Windows平台上，show()方法返回NotificationHandle
                        if let Ok(handle) =
                            <_ as std::convert::TryInto<notify_rust::NotificationHandle>>::try_into(
                                _notification_handle,
                            )
                        {
                            handle.wait_for_action(|action| {
                                match action {
                                    // 处理查看按钮点击
                                    action if action.starts_with("view_") => {
                                        // 从动作中提取文章ID
                                        if let Some(id_str) = action.strip_prefix("view_") {
                                            if let Ok(id) = id_str.parse::<i64>() {
                                                info!("用户要查看文章 ID: {}", id);

                                                // 通过UI消息发送器发送SelectArticle消息
                                                if let Some(sender) = ui_sender_clone.as_ref() {
                                                    if let Err(e) =
                                                        sender.send(UiMessage::SelectArticle(id))
                                                    {
                                                        error!(
                                                            "发送SelectArticle消息失败: {:?}",
                                                            e
                                                        );
                                                    } else {
                                                        info!(
                                                            "已发送SelectArticle消息，文章ID: {}",
                                                            id
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    // 处理忽略按钮点击
                                    "dismiss" => {
                                        info!("用户忽略了通知");
                                    }
                                    // 处理通知自动关闭
                                    "__closed" => {
                                        info!("通知自动关闭");
                                    }
                                    // 处理其他情况
                                    _ => {
                                        info!("未知通知动作: {}", action);
                                    }
                                }
                            });
                        }
                    }

                    // 在Windows平台上，show()方法返回()，不支持wait_for_action
                    #[cfg(target_os = "windows")]
                    {
                        info!("Windows平台不支持通知交互处理");
                    }
                });
            }
            Err(e) => {
                error!("发送通知失败: {:?}, 详细错误信息: {}", e, e);
                // 尝试提供更具体的错误信息
                #[cfg(target_os = "windows")]
                {
                    error!("Windows通知可能需要在系统设置中允许应用显示通知");
                    error!("请检查Windows设置 > 系统 > 通知和操作 > 通知");
                    error!("查找并确保'RustRSSReader.UniqueAppID'应用的通知权限已开启");
                    error!(
                        "注意：Windows可能将通知显示为关联到PowerShell，这是控制台应用的常见行为"
                    );
                    error!(
                        "建议：考虑将应用打包为Windows应用程序，或使用Windows特定的通知API以获得更好的识别"
                    );
                }
            }
        }
    }

    /// 截断文本，确保不会过长
    fn truncate_text(text: &str, max_length: usize) -> String {
        if text.chars().count() <= max_length {
            return text.to_string();
        }

        // 尝试在单词边界截断，避免切断中文
        let mut result = String::new();
        let mut char_count = 0;

        for c in text.chars() {
            char_count += 1;
            if char_count > max_length {
                result.push_str("...");
                break;
            }
            result.push(c);
        }

        result
    }

    /// 通知新文章
    pub fn notify_new_articles(&self, feed_articles: Vec<(String, Article)>) {
        if feed_articles.is_empty() {
            return;
        }

        if let Err(e) = self
            .event_sender
            .send(NotificationEvent::NewArticles(feed_articles))
        {
            error!("发送通知事件失败: {:?}", e);
        }
    }

    /// 更新配置
    pub fn update_config(&self, enabled: bool, max_notifications: usize, timeout_ms: u64) {
        if let Err(e) = self.event_sender.send(NotificationEvent::UpdateConfig(
            enabled,
            max_notifications,
            timeout_ms,
        )) {
            error!("更新通知配置失败: {:?}", e);
        }
    }

    /// 启用通知
    #[allow(unused)]
    pub fn enable(&self) {
        // 使用try_lock避免在非异步上下文中使用await
        if let Ok(config) = self.config.try_lock() {
            self.update_config(true, config.max_notifications, config.timeout_ms);
        } else {
            log::warn!("无法获取通知配置锁，跳过启用通知操作");
        }
    }

    /// 禁用通知
    #[allow(unused)]
    pub fn disable(&self) {
        // 使用try_lock避免在非异步上下文中使用await
        if let Ok(config) = self.config.try_lock() {
            self.update_config(false, config.max_notifications, config.timeout_ms);
        } else {
            log::warn!("无法获取通知配置锁，跳过禁用通知操作");
        }
    }

    /// 设置最大通知数量
    #[allow(unused)]
    pub fn set_max_notifications(&self, max: usize) {
        // 使用try_lock避免在非异步上下文中使用await
        if let Ok(config) = self.config.try_lock() {
            self.update_config(config.enabled, max, config.timeout_ms);
        } else {
            log::warn!("无法获取通知配置锁，跳过设置最大通知数量操作");
        }
    }

    /// 设置通知超时时间
    #[allow(unused)]
    pub fn set_timeout(&self, timeout_ms: u64) {
        // 使用try_lock避免在非异步上下文中使用await
        if let Ok(config) = self.config.try_lock() {
            self.update_config(config.enabled, config.max_notifications, timeout_ms);
        } else {
            log::warn!("无法获取通知配置锁，跳过设置通知超时时间操作");
        }
    }
}

// 实现Drop trait，确保资源正确释放
impl Drop for NotificationManager {
    fn drop(&mut self) {
        // 尝试发送停止信号
        if let Err(e) = self.event_sender.send(NotificationEvent::Stop) {
            warn!("发送通知停止信号失败: {:?}", e);
        }
        // 不需要join线程，因为它会自行退出
        // 但我们需要确保event_sender不会过早被丢弃
        // 当event_sender被丢弃时，receiver.recv()会返回Err，线程会退出
    }
}
