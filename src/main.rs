// 导入必要的依赖
use eframe::egui;
// use std::sync::{Arc, Mutex};
// use std::sync::mpsc;
use tokio::runtime::Runtime;

// Windows特定的控制台控制
#[cfg(target_os = "windows")]
pub fn set_console_visible(visible: bool) {
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

// 非Windows平台的空实现
#[cfg(not(target_os = "windows"))]
fn set_console_visible(_visible: bool) {
    // 在非Windows平台上不执行任何操作
}

// 导入自定义模块
mod ai_client;
mod app;
mod article_processor;
mod config;
mod feed_manager;
mod models;
mod notification;
mod rss;
mod search;
mod storage;
mod test_db_init;
mod tray;
mod utils;

fn main() -> Result<(), eframe::Error> {
    // 检查是否启用测试模式
    let args: Vec<String> = std::env::args().collect();
    let test_mode = args.len() > 1 && args[1] == "--test-auto-update";

    // 如果是测试模式，直接运行测试
    if test_mode {
        println!("进入自动更新功能测试模式...");
        return test_auto_update();
    }

    // 测试数据库初始化
    println!("[数据库测试] 正在检查数据库状态...");
    test_db_init::test_database_initialization();
    println!("[数据库测试] 测试完成\n");

    // 配置日志，设置级别为info以减少调试日志输出 debug
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 创建Tokio运行时以支持异步操作
    let runtime = Runtime::new().expect("Failed to create Tokio runtime");

    // 在Tokio运行时中执行应用逻辑
    runtime.block_on(async {
        // 加载配置以获取系统托盘设置和控制台显示设置
        let config = config::AppConfig::load_or_default();

        // 根据配置控制控制台窗口的显示
        #[cfg(target_os = "windows")]
        {
            println!("[配置] 控制台窗口显示设置: {}", config.show_console);
            set_console_visible(config.show_console);
        }

        // 设置应用程序配置
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([config.window_width as f32, config.window_height as f32])
                .with_min_inner_size([800.0, 600.0]),
            ..Default::default()
        };
        println!("[DEBUG] 已强制设置为亮色主题，用于测试黑屏问题");

        // 启动应用程序
        eframe::run_native(
            "Rust语言编写的RSS阅读器,代码编写->人工智能,设计思路->Chtcrack",
            options,
            Box::new(move |cc| {
                // 配置中文字体支持
                let mut fonts = egui::FontDefinitions::default();

                // 添加详细的字体加载日志
                println!("开始加载中文字体...");

                // 使用绝对路径尝试加载字体，同时尝试多种可能的路径
                // 使用环境变量获取Windows目录，构建系统字体路径，更加通用
                let system_font_path = match std::env::var("WINDIR") {
                    Ok(win_dir) => format!(r"{}\Fonts\msyh.ttc", win_dir),
                    Err(_) => r"C:\Windows\Fonts\msyh.ttc".to_string(), // 失败时使用默认路径作为备选
                };

                let font_paths = [&system_font_path, r".\fonts\msyh.ttf"];
                let mut font_loaded = false;

                for path in &font_paths {
                    println!("尝试加载字体文件: {}", path);
                    match std::fs::read(path) {
                        Ok(font_data) => {
                            println!("成功读取字体文件，大小: {} 字节", font_data.len());

                            fonts.font_data.insert(
                                "microsoft_yahei".to_owned(),
                                egui::FontData::from_owned(font_data).into(),
                            );

                            // 将中文字体添加到字体家族中
                            if let Some(family) =
                                fonts.families.get_mut(&egui::FontFamily::Proportional)
                            {
                                family.insert(0, "microsoft_yahei".to_owned());
                                println!("已将中文字体添加到Proportional字体家族");
                            }

                            // 也添加到等宽字体家族，确保代码和表格中的中文也能正常显示
                            if let Some(family) =
                                fonts.families.get_mut(&egui::FontFamily::Monospace)
                            {
                                family.insert(0, "microsoft_yahei".to_owned());
                                println!("已将中文字体添加到Monospace字体家族");
                            }

                            font_loaded = true;
                            println!("字体加载成功: {}", path);
                            break; // 成功后退出循环
                        }
                        Err(e) => {
                            println!("无法加载字体文件 {}: {}", path, e);
                        }
                    }
                }

                if !font_loaded {
                    println!("错误: 所有字体文件都加载失败");
                    // 显示错误对话框并退出程序
                    egui::Window::new("错误")
                        .collapsible(false)
                        .resizable(false)
                        .show(&cc.egui_ctx, |ui| {
                            ui.label("无法加载字体，程序退出");
                            ui.horizontal(|ui| {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("确定").clicked() {
                                            std::process::exit(1);
                                        }
                                    },
                                );
                            });
                        });
                    // 确保对话框显示
                    cc.egui_ctx.request_repaint();
                    // 等待用户点击确定（程序会在点击时退出）
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }

                // 字体加载成功
                println!("字体设置完成");

                // 应用字体设置
                cc.egui_ctx.set_fonts(fonts);
                println!("字体设置已应用到GUI上下文");

                // 初始化系统托盘（可选）
                let (tray_manager, tray_receiver) = if config.show_tray_icon {
                    log::debug!("系统托盘功能已启用，正在初始化...");
                    // 创建系统托盘管理器
                    match std::panic::catch_unwind(|| {
                        // 初始化系统托盘
                        use crate::tray::TrayManager;
                        use std::sync::Arc;
                        let (tray_manager, tray_receiver) = TrayManager::new(config.show_tray_icon);
                        (Some(Arc::new(tray_manager)), tray_receiver)
                    }) {
                        Ok((tray, receiver)) => {
                            log::info!("系统托盘初始化成功");
                            (tray, receiver)
                        }
                        Err(e) => {
                            log::error!("初始化系统托盘失败: {:?}", e);
                            // 即使托盘初始化失败，程序也能继续运行
                            (None, None)
                        }
                    }
                } else {
                    log::debug!("系统托盘功能已禁用，跳过初始化");
                    (None, None)
                };

                // 根据是否有tray_manager选择不同的应用创建方式
                match tray_manager {
                    Some(tray) => Ok(Box::new(app::App::new_with_tray(cc, tray, tray_receiver))),
                    None => {
                        // 如果没有系统托盘，使用普通的应用创建方式
                        Ok(Box::new(app::App::new(cc)))
                    }
                }
            }),
        )
    })
}

/// 测试自动更新功能
fn test_auto_update() -> Result<(), eframe::Error> {
    // 配置日志，设置级别为info以减少调试日志输出
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 创建Tokio运行时以支持异步操作
    let runtime = Runtime::new().expect("Failed to create Tokio runtime");

    // 在Tokio运行时中执行测试
    runtime.block_on(async {
        println!("开始测试自动更新功能...");

        // 导入必要的模块
        use crate::app::UiMessage;
        use crate::app::perform_auto_update;
        use crate::rss::RssFetcher;
        use crate::search::SearchManager;
        use crate::storage::StorageManager;
        use std::sync::Arc;
        use std::sync::mpsc::channel;
        use std::time::Duration;
        use tokio::sync::Mutex;

        // 创建消息通道
        let (ui_tx, _ui_rx) = channel::<UiMessage>();

        // 初始化存储管理器（使用内存数据库，避免影响实际数据）
        println!("初始化存储管理器...");
        let storage = Arc::new(Mutex::new(StorageManager::new(":memory:".to_string())));

        // 初始化RSS获取器
        println!("初始化RSS获取器...");
        let rss_fetcher = Arc::new(Mutex::new(RssFetcher::new(
            "Rust RSS Reader Test/1.0".to_string(),
        )));

        // 测试1: 执行自动更新
        println!("\n测试1: 执行自动更新...");
        // 初始化搜索管理器
        let search_manager = Some(Arc::new(Mutex::new(SearchManager::new())));
        match perform_auto_update(
            storage.clone(),
            rss_fetcher.clone(),
            None,
            ui_tx.clone(),
            search_manager.clone(),
            None, // AI客户端，测试中不使用
        )
        .await
        {
            Ok(_) => println!("测试1通过: 自动更新执行成功"),
            Err(e) => println!("测试1失败: 自动更新执行失败: {:?}", e),
        }

        // 测试2: 测试任务取消功能
        println!("\n测试2: 测试任务取消功能...");
        let storage_clone = storage.clone();
        let rss_fetcher_clone = rss_fetcher.clone();
        let ui_tx_clone = ui_tx.clone();

        let handle = tokio::spawn(async move {
            println!("启动自动更新任务...");
            // 这里不需要重新创建SearchManager，使用已有的
            perform_auto_update(storage_clone, rss_fetcher_clone, None, ui_tx_clone, None, None).await
        });

        // 等待一小段时间
        tokio::time::sleep(Duration::from_secs(1)).await;

        // 取消任务
        println!("取消自动更新任务...");
        handle.abort();

        // 检查任务状态
        match handle.await {
            Ok(_) => println!("任务正常完成"),
            Err(e) if e.is_cancelled() => println!("测试2通过: 任务被成功取消"),
            Err(e) => println!("测试2失败: 任务出错: {:?}", e),
        }

        println!("\n自动更新功能测试完成！");
        Ok::<(), eframe::Error>(())
    })
}
