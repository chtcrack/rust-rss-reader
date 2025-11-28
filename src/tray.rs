use std::result::Result;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{JoinHandle, sleep};
use std::time::Duration;
use tray_item::TrayItem as TrayItemLib;

// Windows API相关导入（仅在Windows平台使用）

// 系统托盘消息枚举
#[derive(Debug, Clone)]
pub enum TrayMessage {
    ShowWindow, // 用于显示窗口
    HideWindow, // 用于隐藏窗口
    Exit,
    // ShowNotification(String, String), // 暂时未使用
    // 可以添加更多消息类型
}

// 系统托盘配置
#[derive(Debug, Clone)]
enum TrayIconConfig {
    // 尝试多种不同的标题和图标配置，直到成功
    FirstAttempt,
    SecondAttempt,
    ThirdAttempt,
    Fallback,
}

// 尝试创建系统托盘的函数
fn create_tray_item(config: TrayIconConfig) -> Result<TrayItemLib, Box<dyn std::error::Error>> {
    let process_id = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();

    // 生成真正唯一的标题，避免"类已存在"错误
    let title = match config {
        TrayIconConfig::FirstAttempt => {
            format!("RSS_{}_{}", process_id, timestamp)
        }
        TrayIconConfig::SecondAttempt => {
            format!("Reader_{}_{}", process_id, rand::random::<u32>())
        }
        TrayIconConfig::ThirdAttempt => {
            format!("App_{}_{}", timestamp, rand::random::<u16>())
        }
        TrayIconConfig::Fallback => {
            // 使用非常短且唯一的标题
            format!("R{}_{}", process_id % 1000, rand::random::<u8>())
        }
    };

    log::info!(
        "[DEBUG] Creating tray with unique title: {}, config: {:?}",
        title,
        config
    );

    // 尝试多种图标源策略
    // 1. 首先尝试使用Windows API直接加载默认图标，然后通过RawIcon变体传递
    #[cfg(target_os = "windows")]
    {
        {
            use windows_sys::Win32::{
                Foundation::HMODULE,
                UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW},
            };

            unsafe {
                // 加载Windows默认应用图标
                let hicon = LoadIconW(0 as HMODULE, IDI_APPLICATION);
                if hicon != 0 {
                    match TrayItemLib::new(&title, tray_item::IconSource::RawIcon(hicon)) {
                        Ok(tray) => {
                            log::info!(
                                "[DEBUG] Successfully created tray icon with RawIcon (Windows default icon)"
                            );
                            return Ok(tray);
                        }
                        Err(e) => {
                            log::warn!(
                                "[DEBUG] Failed to create tray with RawIcon: {}, error type: {:?}",
                                e,
                                e
                            );
                        }
                    }
                } else {
                    log::warn!("[DEBUG] Failed to load default Windows icon using LoadIconW");
                }
            }
        }
    }

    // 2. 如果RawIcon失败，尝试使用资源名称"IDI_APPLICATION"
    match TrayItemLib::new(&title, tray_item::IconSource::Resource("IDI_APPLICATION")) {
        Ok(tray) => {
            log::info!("[DEBUG] Successfully created tray icon with resource name IDI_APPLICATION");
            Ok(tray)
        }
        Err(e) => {
            log::warn!(
                "[DEBUG] Failed to create tray with resource name IDI_APPLICATION: {}, error type: {:?}",
                e,
                e
            );
            // 3. 最后尝试使用空字符串作为资源名称（原始实现）
            match TrayItemLib::new(&title, tray_item::IconSource::Resource("")) {
                Ok(tray) => {
                    log::info!("[DEBUG] Successfully created tray icon with empty resource string");
                    Ok(tray)
                }
                Err(e) => {
                    log::warn!(
                        "[DEBUG] Failed to create tray with empty resource string: {}, error type: {:?}",
                        e,
                        e
                    );
                    Err(Box::new(e))
                }
            }
        }
    }
}

// 运行系统托盘线程的独立函数
fn run_tray_thread(
    rx: Receiver<TrayMessage>,
    tx: Sender<TrayMessage>,
    tx_app: Sender<TrayMessage>,
    running: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("[DEBUG] Starting tray thread");

    // 尝试多种配置创建系统托盘
    let mut tray = None;

    // 增加重试间隔，使用递增的等待时间
    let mut retry_delay = 100;

    // 尝试不同的配置，直到成功或全部失败
    for config in [
        TrayIconConfig::FirstAttempt,
        TrayIconConfig::SecondAttempt,
        TrayIconConfig::ThirdAttempt,
        TrayIconConfig::Fallback,
    ] {
        match create_tray_item(config.clone()) {
            Ok(t) => {
                tray = Some(t);
                break;
            }
            Err(e) => {
                log::warn!(
                    "Failed to create tray with config: {:?}, error: {}",
                    config,
                    e
                );
                // 递增等待时间，避免频繁重试
                sleep(Duration::from_millis(retry_delay));
                retry_delay *= 2;
            }
        }
    }

    // 如果创建托盘失败，仍然让线程继续运行，只是没有托盘功能
    // 这样程序的其他功能仍然可以正常工作
    if tray.is_none() {
        log::warn!("[DEBUG] System tray creation failed, running in headless mode");
    }

    // 不使用?操作符，而是使用Option
    let mut tray = tray;

    // 创建临时通道用于菜单项回调
    let (tx_menu, rx_menu) = channel::<TrayMessage>();
    let tx_menu_arc = Arc::new(tx_menu);

    // 只有当tray创建成功时才添加菜单项
    if let Some(ref mut tray_ref) = tray {
        log::info!("[DEBUG] Adding menu items to tray");

        // 添加菜单项
        // 使用闭包捕获tx_menu_arc的克隆
        if let Err(e) = tray_ref.add_menu_item("显示窗口", {
            let tx = tx_menu_arc.clone();
            move || {
                if let Err(e) = tx.send(TrayMessage::ShowWindow) {
                    log::error!("Failed to send show window message: {}", e);
                }
            }
        }) {
            log::error!("Failed to add show window menu item: {}", e);
        }

        if let Err(e) = tray_ref.add_menu_item("隐藏窗口", {
            let tx = tx_menu_arc.clone();
            move || {
                if let Err(e) = tx.send(TrayMessage::HideWindow) {
                    log::error!("Failed to send hide window message: {}", e);
                }
            }
        }) {
            log::error!("Failed to add hide window menu item: {}", e);
        }

        // 有些tray_item版本可能不支持add_separator
        // 注释掉此功能以保持兼容性
        // tray_ref.add_separator()?;

        // 添加退出菜单项，直接使用tx_app发送退出信号给App
        if let Err(e) = tray_ref.add_menu_item("退出", {
            let tx = tx_app.clone();
            move || {
                if let Err(e) = tx.send(TrayMessage::Exit) {
                    log::error!("Failed to send exit message: {}", e);
                }
            }
        }) {
            log::error!("Failed to add exit menu item: {}", e);
        }
    } else {
        log::info!("[DEBUG] Tray not available, skipping menu item setup");
    }

    // 主消息循环
    const CHECK_INTERVAL: Duration = Duration::from_millis(50);
    const LONG_TIMEOUT: Duration = Duration::from_millis(500); // 从1秒改为500毫秒，提高响应性
    const IDLE_TIMEOUT: Duration = Duration::from_secs(5); // 长时间空闲时使用更长的等待时间

    // 跟踪活动状态，用于动态调整CPU使用率
    let mut consecutive_idle_count = 0;
    const MAX_IDLE_COUNT: u32 = 10; // 连续空闲10次后进入低功耗模式

    log::info!("[DEBUG] Starting tray message loop with optimized CPU usage");
    loop {
        // 首先检查运行标志
        if !running.load(Ordering::Relaxed) {
            log::info!("[DEBUG] Tray thread stopping due to shutdown flag");
            break Ok(());
        }

        // 检查是否有菜单项消息
        let mut has_activity = false;
        let menu_msgs: Vec<_> = rx_menu.try_iter().collect();

        for msg in menu_msgs {
            has_activity = true;
            consecutive_idle_count = 0; // 重置空闲计数
            match msg {
                TrayMessage::ShowWindow | TrayMessage::HideWindow | TrayMessage::Exit => {
                    // 转发所有菜单项消息到应用程序
                    if let Err(e) = tx_app.send(msg.clone()) {
                        log::error!("Failed to forward menu message to app: {}", e);
                    }
                    // 同时转发到内部通道
                    if let Err(e) = tx.send(msg.clone()) {
                        log::error!("Failed to forward menu message internally: {}", e);
                    }
                }
            }
        }

        // 确定当前的等待超时时间
        let timeout = if consecutive_idle_count > MAX_IDLE_COUNT {
            // 长时间空闲时使用最长等待时间
            log::debug!("[DEBUG] System idle, using longer sleep to reduce CPU usage");
            IDLE_TIMEOUT
        } else if has_activity {
            // 有活动时使用短等待时间
            CHECK_INTERVAL
        } else {
            // 正常情况下使用中等等待时间
            LONG_TIMEOUT
        };

        // 检查是否有系统托盘消息
        match rx.recv_timeout(timeout) {
            Ok(msg) => {
                // has_activity = true; // 暂时注释掉未使用的赋值
                consecutive_idle_count = 0; // 重置空闲计数

                match msg {
                    TrayMessage::Exit => {
                        log::info!("[DEBUG] Received exit message in tray thread");
                        break Ok(());
                    }
                    _ => {}
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // 超时是正常的，增加空闲计数
                consecutive_idle_count += 1;
            }
            Err(e) => {
                log::error!("Error receiving tray message: {}", e);
                break Ok(()); // 出错时退出循环
            }
        }
    }
}

// 系统托盘管理器
pub struct TrayManager {
    tx: Sender<TrayMessage>,
    rx: Option<Receiver<TrayMessage>>, // 保存接收器，用于App接收托盘消息
    thread_handle: Option<JoinHandle<()>>,
    enabled: bool,            // 标识托盘是否启用
    running: Arc<AtomicBool>, // 线程运行标志
}

impl Clone for TrayManager {
    fn clone(&self) -> Self {
        // 创建一个新的通道，但保留对原始功能的访问
        let (tx, _) = channel();
        Self {
            tx: tx,
            rx: None,
            thread_handle: None,
            enabled: self.enabled,
            running: Arc::new(AtomicBool::new(false)), // 克隆不启动新线程
        }
    }
}

impl TrayManager {
    // 创建新的系统托盘管理器
    pub fn new(enabled: bool) -> (Self, Option<Receiver<TrayMessage>>) {
        log::info!("[TrayManager] Creating TrayManager, enabled: {}", enabled);
        if !enabled {
            let (tx, _) = channel();
            return (
                Self {
                    tx,
                    rx: None,
                    thread_handle: None,
                    enabled: false,
                    running: Arc::new(AtomicBool::new(false)),
                },
                None,
            );
        }

        // 创建运行标志
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // 创建两个通道：一个用于托盘线程内部通信，一个用于App接收退出消息
        let (tx_tray, rx_tray) = channel::<TrayMessage>();
        let (tx_app, rx_app) = channel::<TrayMessage>();

        // 启动系统托盘线程
        log::debug!("[TrayManager] 启动系统托盘线程，enabled: {}", enabled);
        let tx_tray_clone = tx_tray.clone();
        let tx_app_clone = tx_app.clone();
        let thread_handle = std::thread::spawn(move || {
            // 使用catch_unwind确保托盘线程崩溃不会影响主程序
            if let Err(e) = std::panic::catch_unwind(|| {
                // 直接调用函数，而不是通过Self::
                if let Err(e) = run_tray_thread(rx_tray, tx_tray_clone, tx_app_clone, running_clone)
                {
                    log::error!("[TrayManager] Tray thread error: {}", e);
                }
            }) {
                log::error!("[TrayManager] 系统托盘线程崩溃: {:?}", e);
            }
        });

        log::info!("[TrayManager] 系统托盘线程启动完成");
        (
            Self {
                tx: tx_tray,
                rx: None,
                thread_handle: Some(thread_handle),
                enabled: true,
                running,
            },
            Some(rx_app),
        ) // 返回管理器和App的接收器
    }

    // 显示通知
    // fn show_notification_static(title: &str, body: &str) { // 暂时注释，未使用
    //     log::info!("Notification: {} - {}", title, body);
    //
    //     // 尝试使用Windows API显示通知
    //     #[cfg(target_os = "windows")]
    //     unsafe {
    //         use winapi::um::winuser::{MessageBoxW, MB_OK, MB_ICONINFORMATION};
    //
    //         // 将字符串转换为UTF-16
    //         let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    //         let body_wide: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    //
    //         // 显示简单的消息框作为通知替代
    //         MessageBoxW(
    //             std::ptr::null_mut(),
    //             body_wide.as_ptr(),
    //             title_wide.as_ptr(),
    //             MB_OK | MB_ICONINFORMATION,
    //         );
    //     }
    // }

    // 显示窗口
    // pub fn show_window(&self) { // 暂时注释，未使用
    //     if !self.enabled {
    //         return;
    //     }
    //
    //     if let Err(e) = self.tx.send(TrayMessage::ShowWindow) {
    //         log::error!("Failed to send show window message: {}", e);
    //     }
    // }

    // 隐藏窗口
    // pub fn hide_window(&self) { // 暂时注释，未使用
    //     if !self.enabled {
    //         return;
    //     }
    //
    //     if let Err(e) = self.tx.send(TrayMessage::HideWindow) {
    //         log::error!("Failed to send hide message: {}", e);
    //     }
    // }

    // 以下方法暂时未使用
    /*
    // 显示通知
    pub fn show_notification(&self, title: &str, body: &str) {
        if !self.enabled {
            return;
        }

        if let Err(e) = self.tx.send(TrayMessage::ShowNotification(
            title.to_string(),
            body.to_string(),
        )) {
            log::error!("Failed to send notification message: {}", e);
        }
    }

    // 尝试接收托盘消息
    pub fn try_recv_message(&self) -> Option<TrayMessage> {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(msg) => Some(msg),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(e) => {
                    log::error!("Failed to receive tray message: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }
    */
}

// 简单通知实现，不依赖外部库
// fn show_basic_notification(title: &str, body: &str) { // 暂时注释，未使用
//     log::info!("Notification: {} - {}", title, body);
//     // 在Windows上，可以使用Windows API显示通知
//     // 这里保持简单实现，记录日志
// }

// 实现Drop trait，确保线程正确终止
impl Drop for TrayManager {
    fn drop(&mut self) {
        log::info!("TrayManager being dropped, cleaning up resources");

        // 设置运行标志为false，通知线程停止
        if self.running.load(Ordering::Relaxed) {
            log::info!("Setting tray thread shutdown flag");
            self.running.store(false, Ordering::Relaxed);
        }

        // 释放接收器
        self.rx.take();

        // 尝试发送退出消息
        if self.enabled {
            log::info!("Sending exit message to tray thread");
            let _ = self.tx.send(TrayMessage::Exit);
        }

        // 等待线程结束，但不无限阻塞
        if let Some(handle) = self.thread_handle.take() {
            log::info!("Waiting for tray thread to finish");
            match handle.join() {
                Ok(_) => log::info!("Tray thread exited successfully"),
                Err(e) => log::error!("Tray thread panicked: {:?}", e),
            }
        }

        log::info!("TrayManager cleanup complete");
    }
}
