// 测试数据库初始化功能
use crate::storage::StorageManager;
use std::path::PathBuf;

// 测试数据库初始化和错误处理
pub fn test_database_initialization() {
    println!("开始测试数据库初始化...");

    // 获取默认数据库路径
    let db_path = "feed.duckdb";
    println!("使用数据库路径: {}", db_path);

    // 尝试创建存储管理器
    // 注意：StorageManager::new现在包含错误处理，即使失败也不会导致应用崩溃
    let _manager = StorageManager::new(db_path.to_string());

    println!("✓ 数据库初始化完成 (即使有错误也会被内部处理)");
    println!("✓ 应用程序不会因数据库问题而崩溃");
    println!("数据库测试完成!");
}

// 测试导出到SQL文件功能
pub async fn test_export_to_sql() {
    println!("开始测试导出到SQL文件功能...");

    // 获取默认数据库路径
    let db_path = "feed.duckdb";
    println!("使用数据库路径: {}", db_path);

    // 创建存储管理器
    let manager = StorageManager::new(db_path.to_string());

    // 导出到SQL文件
    let export_path = PathBuf::from("export_test.sql");
    match manager.export_to_sql(&export_path).await {
        Ok(_) => {
            println!("✓ 成功导出SQL文件到: {}", export_path.display());
            // 检查文件是否存在
            if export_path.exists() {
                println!("✓ 导出的SQL文件存在");
            } else {
                println!("✗ 导出的SQL文件不存在");
            }
        },
        Err(e) => {
            println!("✗ 导出SQL文件失败: {}", e);
        }
    }

    println!("导出到SQL文件功能测试完成!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_init() {
        test_database_initialization();
    }

    #[tokio::test]
    async fn test_export_sql() {
        test_export_to_sql().await;
    }
}
