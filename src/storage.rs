// 数据存储模块

use crate::models::{Article, Feed, FeedGroup};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Result as SqliteResult, params};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use ammonia::{Builder, UrlRelative};
use html_escape::decode_html_entities;

/// 存储管理器
pub struct StorageManager {
    /// 数据库连接
    connection: Arc<Mutex<Connection>>,
}

impl StorageManager {
    /// 清理HTML内容，只保留p和br标签，移除所有其他HTML标签
    fn sanitize_html(html: &str) -> String {
        // 首先检查输入是否为空
        if html.is_empty() {
            return String::new();
        }
        
        // 尝试处理HTML内容，添加错误处理
        match std::panic::catch_unwind(|| {
            // 创建ammonia构建器
            let mut builder = Builder::new();
            
            // 只允许p和br标签
            let allowed_tags = HashSet::from(["p", "br"]);
            builder.tags(allowed_tags);
            
            // 不允许任何属性
            builder.tag_attributes(HashMap::new());
            
            // 不允许任何协议处理
            builder.url_relative(UrlRelative::PassThrough);
            
            // 不允许任何链接处理
            builder.link_rel(None);
            
            // 清理HTML内容
            let sanitized_html = builder.clean(html).to_string();
            
            // 解码HTML实体
            let decoded = decode_html_entities(&sanitized_html);
            
            // 移除多余的空白字符，保留合理的换行和空格
            let cleaned = decoded
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            
            cleaned
        }) {
            Ok(cleaned_content) => cleaned_content,
            Err(_) => {
                // 如果处理失败，尝试使用更简单的方式处理
                log::warn!("HTML清理失败，使用简单处理方式");
                // 解码HTML实体
                let decoded = decode_html_entities(html);
                // 移除HTML标签
                let simple_cleaned = regex::Regex::new(r"<[^>]*>")
                    .unwrap_or_else(|_| regex::Regex::new(r"").unwrap())
                    .replace_all(&decoded, "")
                    .to_string();
                // 移除多余的空白字符
                simple_cleaned
                    .lines()
                    .map(|line| line.trim())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    /// 创建新的存储管理器
    pub fn new(db_path: String) -> Self {
        // 尝试连接数据库，如果失败则尝试创建
        let conn = match Connection::open(&db_path) {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("警告: 无法打开数据库 {:?}: {}", db_path, e);
                eprintln!("尝试创建新的数据库文件...");
                // 确保数据库目录存在
                if let Some(parent) = PathBuf::from(&db_path).parent() {
                    if let Err(create_err) = std::fs::create_dir_all(parent) {
                        eprintln!("错误: 无法创建数据库目录: {}", create_err);
                    }
                }
                // 再次尝试打开数据库
                Connection::open(db_path).expect("创建数据库失败")
            }
        };

        // 初始化数据库表，添加错误处理
        match Self::init_database(&conn) {
            Ok(_) => (),
            Err(e) => {
                eprintln!("警告: 数据库表初始化时出现警告: {}", e);
                // 即使初始化有警告也继续运行，尽量恢复
            }
        }

        // 设置自动VACUUM模式，使SQLite在每次删除提交时自动回收空间
        if let Err(e) = conn.execute("PRAGMA auto_vacuum = 1;", []) {
            eprintln!("警告: 无法设置自动VACUUM模式: {}", e);
        }
        // 2. 立即执行一次完整VACUUM来回收现有空间
        if let Err(e) = conn.execute("VACUUM;", []) {
            eprintln!("警告: 无法执行VACUUM: {}", e);
        }
        // 3. 其他优化设置
        // 使用WAL模式，提高并发性能和崩溃恢复能力
        let _ = conn.execute("PRAGMA journal_mode = WAL;", []);
        // 设置synchronous=NORMAL，平衡性能和安全性
        let _ = conn.execute("PRAGMA synchronous = NORMAL;", []);
        // 设置WAL自动检查点阈值为1000页，控制WAL文件大小
        let _ = conn.execute("PRAGMA wal_autocheckpoint = 1000;", []);
        // 启用WAL校验和，提高数据完整性
        let _ = conn.execute("PRAGMA wal_checkpoint(TRUNCATE);", []);
        // 启用数据完整性检查
        let _ = conn.execute("PRAGMA integrity_check;", []);

        Self {
            connection: Arc::new(Mutex::new(conn)),
        }
    }

    /// 初始化数据库表
    fn init_database(conn: &Connection) -> SqliteResult<()> {
        // 创建订阅源分组表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS feed_groups (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                icon TEXT
            )"#,
            [],
        )?;

        // 创建订阅源表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS feeds (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                url TEXT NOT NULL UNIQUE,
                group_name TEXT DEFAULT '',
                last_updated TIMESTAMP,
                description TEXT,
                language TEXT,
                link TEXT,
                favicon TEXT,
                auto_update INTEGER DEFAULT 1,
                enable_notification INTEGER DEFAULT 1,
                ai_auto_translate INTEGER DEFAULT 0
            )"#,
            [],
        )?;

        // 检查并添加auto_update字段（如果不存在）
        let mut stmt = conn.prepare("PRAGMA table_info(feeds)")?;
        let table_info: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))? // 索引1对应列名
            .collect::<SqliteResult<_>>()?;

        let has_auto_update = table_info.iter().any(|col| col == "auto_update");
        if !has_auto_update {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN auto_update INTEGER DEFAULT 1",
                [],
            )?;
        }

        // 检查并添加enable_notification字段（如果不存在）
        let has_enable_notification = table_info.iter().any(|col| col == "enable_notification");
        if !has_enable_notification {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN enable_notification INTEGER DEFAULT 1",
                [],
            )?;
        }

        // 检查并添加ai_auto_translate字段（如果不存在）
        let has_ai_auto_translate = table_info.iter().any(|col| col == "ai_auto_translate");
        if !has_ai_auto_translate {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN ai_auto_translate INTEGER DEFAULT 0",
                [],
            )?;
        }

        // 检查并添加group_id字段（如果不存在）
        let has_group_id = table_info.iter().any(|col| col == "group_id");
        if !has_group_id {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN group_id INTEGER DEFAULT NULL",
                [],
            )?;
        }

        // 创建文章表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS articles (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                feed_id INTEGER NOT NULL,
                title TEXT NOT NULL,
                link TEXT,
                author TEXT,
                pub_date TIMESTAMP NOT NULL,
                content TEXT,
                summary TEXT,
                is_read INTEGER DEFAULT 0,
                is_starred INTEGER DEFAULT 0,
                source TEXT,
                guid TEXT,
                FOREIGN KEY (feed_id) REFERENCES feeds(id) ON DELETE CASCADE
            )"#,
            [],
        )?;

        // 创建索引以提高性能
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_feed_id ON articles(feed_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_read ON articles(is_read)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_starred ON articles(is_starred)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_date ON articles(pub_date)",
            [],
        )?;
        // 添加唯一索引，确保相同GUID的文章不会重复插入
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_articles_guid ON articles(guid)",
            [],
        )?;
        // 添加索引以提高重复检查的性能
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_link_feed ON articles(link, feed_id)",
            [],
        )?;
        
        // 为feeds表的group_id字段添加索引，提高分组查询性能
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_feeds_group_id ON feeds(group_id)",
            [],
        )?;

        // 创建设置表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )"#,
            [],
        )?;

        Ok(())
    }

    /// 添加新的订阅源
    #[allow(unused)]
    pub async fn add_feed(&mut self, feed: Feed) -> anyhow::Result<i64> {
        let mut conn = self.connection.lock().await;
        
        // 检查是否已存在相同URL的订阅源
        { // 新作用域，确保stmt在事务开始前被删除
            let mut stmt = conn.prepare("SELECT id FROM feeds WHERE url = ?")?;
            if let Ok(Some(id)) = stmt
                .query_row(params![&feed.url], |row| row.get(0))
                .optional()
            {
                return Ok(id); // 返回已存在的ID
            }
        }
        
        // 开始事务
        let tx = conn.transaction()?;
        
        // 处理分组逻辑
        let group_id = if feed.group.is_empty() {
            // 空分组，使用NULL
            None
        } else {
            // 查询是否已存在同名分组
            let mut group_stmt = tx.prepare("SELECT id FROM feed_groups WHERE name = ?")?;
            let group_exists = group_stmt
                .query_row(params![&feed.group], |row| row.get::<_, i64>(0))
                .optional()?;
            
            if let Some(id) = group_exists {
                Some(id)
            } else {
                // 不存在，插入新分组
                tx.execute(
                    "INSERT OR IGNORE INTO feed_groups (name, icon) VALUES (?, ?)",
                    params![feed.group, None::<&str>],
                )?;
                
                // 获取插入的ID或已存在的ID
                let new_group_id = tx.query_row(
                    "SELECT id FROM feed_groups WHERE name = ?",
                    params![&feed.group],
                    |row| row.get::<_, i64>(0),
                )?;
                
                Some(new_group_id)
            }
        };

        // 添加新订阅源，包含last_updated字段
        let _result = tx.execute(
            "INSERT INTO feeds (title, url, group_id, description, language, link, favicon, last_updated, auto_update, enable_notification, ai_auto_translate) 
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                feed.title,
                feed.url,
                group_id,
                feed.description,
                feed.language,
                feed.link,
                feed.favicon,
                feed.last_updated.unwrap_or_else(chrono::Utc::now), // 使用当前时间作为默认值
                feed.auto_update as i8,
                feed.enable_notification as i8,
                feed.ai_auto_translate as i8
            ],
        )?;

        // 获取插入的ID
        let id = tx.last_insert_rowid();
        
        // 提交事务
        tx.commit()?;

        Ok(id)
    }

    /// 获取所有订阅源

    pub async fn get_all_feeds(&self) -> anyhow::Result<Vec<Feed>> {
        let conn = self.connection.lock().await;

        let mut stmt = conn.prepare(
            "SELECT f.id, f.title, f.url, COALESCE(g.name, '') as group_name, f.group_id, f.last_updated, f.description, f.language, f.link, f.favicon, f.auto_update, f.enable_notification, f.ai_auto_translate 
             FROM feeds f LEFT JOIN feed_groups g ON f.group_id = g.id ORDER BY g.name, f.title"
        )?;

        let feeds = stmt
            .query_map([], |row| {
                Ok(Feed {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    url: row.get(2)?,
                    group: row.get(3)?,
                    group_id: row.get(4)?,
                    last_updated: row.get(5)?,
                    description: row.get(6)?,
                    language: row.get(7)?,
                    link: row.get(8)?,
                    favicon: row.get(9)?,
                    auto_update: row.get(10)?,
                    enable_notification: row.get(11)?,
                    ai_auto_translate: row.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<Feed>>>()?;

        Ok(feeds)
    }

    /// 执行数据迁移，将group_name映射为group_id
    pub async fn migrate_feed_group_ids(&mut self) -> anyhow::Result<()> {
        let mut conn = self.connection.lock().await;
        
        // 开始事务
        let tx = conn.transaction()?;
        
        // 1. 首先获取feeds表中所有唯一的group_name值
        let unique_group_names = {
            let mut stmt = tx.prepare("SELECT DISTINCT group_name FROM feeds WHERE group_name != ''")?;
            stmt
                .query_map([], |row| {
                    Ok(row.get::<_, String>(0)?)
                })?
                .collect::<rusqlite::Result<Vec<String>>>()?
        };
        
        // 2. 确保feed_groups表中存在所有这些分组
        let mut group_name_to_id: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        
        // 先获取现有的分组
        let existing_groups = {
            let mut stmt = tx.prepare("SELECT id, name FROM feed_groups")?;
            stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<(i64, String)>>>()?
        };
        
        // 添加到映射表
        for (id, name) in existing_groups {
            group_name_to_id.insert(name, id);
        }
        
        // 3. 为不存在的分组创建记录
        for group_name in &unique_group_names {
            if !group_name_to_id.contains_key(group_name) && !group_name.is_empty() {
                // 插入新分组
                tx.execute(
                    "INSERT INTO feed_groups (name) VALUES (?)",
                    params![group_name],
                )?;
                
                // 获取插入的ID
                let group_id = tx.last_insert_rowid();
                
                // 添加到映射表
                group_name_to_id.insert(group_name.clone(), group_id);
            }
        }
        
        // 4. 遍历所有订阅源，更新group_id
        let feeds = {
            let mut feed_stmt = tx.prepare("SELECT id, group_name FROM feeds WHERE group_id IS NULL OR group_id = 0")?;
            feed_stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<(i64, String)>>>()?
        };
        
        for (feed_id, group_name) in feeds {
            if let Some(group_id) = group_name_to_id.get(&group_name) {
                // 更新feed的group_id
                tx.execute(
                    "UPDATE feeds SET group_id = ? WHERE id = ?",
                    params![group_id, feed_id],
                )?;
            }
        }
        
        // 提交事务
        tx.commit()?;
        
        Ok(())
    }
    
    /// 清理工作：移除feeds表中的group_name字段
    
    pub async fn cleanup_group_name_field(&mut self) -> anyhow::Result<()> {
        let  conn = self.connection.lock().await;
        
        // 检查group_name字段是否存在
        let mut stmt = conn.prepare("PRAGMA table_info(feeds)")?;
        let table_info: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        
        if table_info.contains(&"group_name".to_string()) {
            // 移除group_name字段
            conn.execute("ALTER TABLE feeds DROP COLUMN group_name", [])?;
            log::info!("成功移除feeds表中的group_name字段");
        } else {
            log::info!("feeds表中不存在group_name字段，无需移除");
        }
        
        Ok(())
    }

    /// 更新订阅源信息

    pub async fn update_feed(&mut self, feed: &Feed) -> anyhow::Result<()> {
        let mut conn = self.connection.lock().await;
        
        // 开始事务
        let tx = conn.transaction()?;
        
        // 处理分组逻辑
        let group_id = if feed.group.is_empty() {
            // 空分组，使用NULL
            None
        } else {
            // 查询是否已存在同名分组
            let mut group_stmt = tx.prepare("SELECT id FROM feed_groups WHERE name = ?")?;
            let group_exists = group_stmt
                .query_row(params![&feed.group], |row| row.get::<_, i64>(0))
                .optional()?;
            
            if let Some(id) = group_exists {
                Some(id)
            } else {
                // 不存在，插入新分组
                tx.execute(
                    "INSERT OR IGNORE INTO feed_groups (name, icon) VALUES (?, ?)",
                    params![feed.group, None::<&str>],
                )?;
                
                // 获取插入的ID或已存在的ID
                let new_group_id = tx.query_row(
                    "SELECT id FROM feed_groups WHERE name = ?",
                    params![&feed.group],
                    |row| row.get::<_, i64>(0),
                )?;
                
                Some(new_group_id)
            }
        };

        tx.execute(
            "UPDATE feeds SET title = ?, url = ?, group_id = ?, description = ?, 
             language = ?, link = ?, favicon = ?, auto_update = ?, enable_notification = ?, ai_auto_translate = ? WHERE id = ?",
            params![
                feed.title,
                feed.url,
                group_id,
                feed.description,
                feed.language,
                feed.link,
                feed.favicon,
                feed.auto_update as i8,
                feed.enable_notification as i8,
                feed.ai_auto_translate as i8,
                feed.id
            ],
        )?;
        
        // 提交事务
        tx.commit()?;

        Ok(())
    }

    /// 更新订阅源最后更新时间

    pub async fn update_feed_last_updated(&mut self, feed_id: i64) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        conn.execute(
            "UPDATE feeds SET last_updated = ? WHERE id = ?",
            params![Utc::now(), feed_id],
        )?;

        Ok(())
    }

    /// 删除订阅源

    pub async fn delete_feed(&mut self, feed_id: i64) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        conn.execute("DELETE FROM feeds WHERE id = ?", params![feed_id])?;

        Ok(())
    }

    /// 获取所有订阅源分组

    pub async fn get_all_groups(&self) -> anyhow::Result<Vec<FeedGroup>> {
        let conn = self.connection.lock().await;

        let mut stmt = conn.prepare("SELECT id, name, icon FROM feed_groups ORDER BY name")?;

        let groups = stmt
            .query_map([], |row| {
                Ok(FeedGroup {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    icon: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<FeedGroup>>>()?;

        Ok(groups)
    }

    /// 删除分组
    #[allow(unused)]
    pub async fn delete_group(&mut self, group_id: i64) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        // 先将使用此分组的订阅源的group_id设为NULL
        conn.execute(
            "UPDATE feeds SET group_id = NULL WHERE group_id = ?",
            params![group_id],
        )?;

        // 删除分组
        conn.execute("DELETE FROM feed_groups WHERE id = ?", params![group_id])?;

        Ok(())
    }

    /// 添加分组
    #[allow(unused)]
    pub async fn add_group(&mut self, group_name: &str, icon: Option<&str>) -> anyhow::Result<i64> {
        let conn = self.connection.lock().await;

        // 验证分组名称不为空
        if group_name.trim().is_empty() {
            return Err(anyhow::anyhow!("分组名称不能为空"));
        }

        // 插入分组，使用INSERT OR IGNORE避免重复
        conn.execute(
            "INSERT OR IGNORE INTO feed_groups (name, icon) VALUES (?, ?)",
            params![group_name.trim(), icon],
        )?;

        // 获取插入的ID或已存在的ID
        let group_id = conn.query_row(
            "SELECT id FROM feed_groups WHERE name = ?",
            params![group_name.trim()],
            |row| row.get(0),
        )?;

        Ok(group_id)
    }

    /// 更新分组名称
    #[allow(unused)]
    pub async fn update_group_name(
        &mut self,
        group_id: i64,
        new_name: &str,
    ) -> anyhow::Result<()> {
        log::debug!("[update_group_name] 开始执行，group_id: {}, new_name: '{}'", group_id, new_name);
        
      let conn = self.connection.lock().await;
        
        // 更新订阅源的group_id
        conn.execute(
            "UPDATE feed_groups SET name = ? WHERE id = ?",
            params![new_name, group_id],
        )?;
        
        
        Ok(())
    }
    
    /// 通过group_id更新订阅源的分组信息
    #[allow(unused)]
    pub async fn update_feed_group(&mut self, feed_id: i64, group_id: Option<i64>) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;
        
        // 更新订阅源的group_id
        conn.execute(
            "UPDATE feeds SET group_id = ? WHERE id = ?",
            params![group_id, feed_id],
        )?;
        
        Ok(())
    }

    /// 保存设置
    #[allow(unused)]
    pub async fn save_setting<T: Serialize>(&mut self, key: &str, value: &T) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;
        let json_value = serde_json::to_string(value)?;

        // 使用INSERT OR REPLACE
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)",
            params![key, json_value],
        )?;

        Ok(())
    }

    /// 获取设置
    #[allow(unused)]
    pub async fn get_setting<T: for<'a> Deserialize<'a>>(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<T>> {
        let conn = self.connection.lock().await;

        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?")?;
        if let Ok(json_value) = stmt.query_row(params![key], |row| row.get::<_, String>(0)) {
            let value: T = serde_json::from_str(&json_value)?;
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    /// 设置自动更新启用状态
    #[allow(unused)]
    pub async fn set_auto_update_enabled(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.save_setting("auto_update_enabled", &enabled).await
    }

    /// 获取更新间隔（分钟）
    #[allow(unused)]
    pub async fn get_update_interval(&self) -> anyhow::Result<u64> {
        const DEFAULT_INTERVAL: u64 = 30; // 默认30分钟
        const SETTING_KEY: &str = "update_interval_minutes";

        if let Some(interval) = self.get_setting::<u64>(SETTING_KEY).await? {
            Ok(interval)
        } else {
            // 如果没有设置，返回默认值
            Ok(DEFAULT_INTERVAL)
        }
    }

    /// 设置更新间隔（分钟）
    #[allow(unused)]
    pub async fn set_update_interval(&mut self, interval: u64) -> anyhow::Result<()> {
        const SETTING_KEY: &str = "update_interval_minutes";

        // 验证间隔值的合理性（1-1440分钟，即1分钟到1天）
        if interval < 1 || interval > 1440 {
            anyhow::bail!("更新间隔必须在1-1440分钟之间");
        }

        self.save_setting(SETTING_KEY, &interval).await
    }

    /// 获取所有收藏的文章
    #[allow(unused)]
    pub async fn get_starred_articles(&self) -> anyhow::Result<Vec<Article>> {
        let conn = self.connection.lock().await;

        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles WHERE is_starred = 1 ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map([], |row| {
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date: row.get(5)?,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get::<_, i32>(8)? == 1,
                    is_starred: row.get::<_, i32>(9)? == 1,
                    source: row.get(10)?,
                    guid: row
                        .get::<_, String>(11)
                        .unwrap_or_else(|_| row.get::<_, String>(3).unwrap_or_default()),
                })
            })?
            .collect::<rusqlite::Result<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 获取所有文章
    #[allow(unused)]
    pub async fn get_all_articles(&self) -> anyhow::Result<Vec<Article>> {
        let conn = self.connection.lock().await;

        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map([], |row| {
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date: row.get(5)?,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get::<_, i32>(8)? == 1,
                    is_starred: row.get::<_, i32>(9)? == 1,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 搜索文章
    #[allow(unused)]
    pub async fn search_articles(&self, query: &str) -> anyhow::Result<Vec<Article>> {
        let conn = self.connection.lock().await;

        let search_pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles 
             WHERE title LIKE ? OR content LIKE ? OR summary LIKE ? 
             ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map(
                params![&search_pattern, &search_pattern, &search_pattern],
                |row| {
                    Ok(Article {
                        id: row.get(0)?,
                        feed_id: row.get(1)?,
                        title: row.get(2)?,
                        link: row.get(3)?,
                        author: row.get(4)?,
                        pub_date: row.get(5)?,
                        content: row.get(6)?,
                        summary: row.get(7)?,
                        is_read: row.get::<_, i32>(8)? == 1,
                        is_starred: row.get::<_, i32>(9)? == 1,
                        source: row.get(10)?,
                        guid: row.get(11)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 清理旧文章（保留最近N篇）
    #[allow(unused)]
    pub async fn cleanup_old_articles(
        &mut self,
        feed_id: i64,
        keep_count: u32,
    ) -> anyhow::Result<()> {
        let mut conn = self.connection.lock().await;
        let tx = conn.transaction()?;

        // 简化删除逻辑，直接使用feed_id和LIMIT
        let query = "DELETE FROM articles WHERE feed_id = ? AND id NOT IN (SELECT id FROM articles WHERE feed_id = ? ORDER BY pub_date DESC LIMIT ?)";
        tx.execute(query, [feed_id, feed_id, keep_count as i64])?;

        tx.commit()?;
        Ok(())
    }

    #[allow(unused)]
    pub async fn export_opml(&self) -> anyhow::Result<String> {
        // 导出OPML文件
        let feeds = self.get_all_feeds().await?;
        let mut opml = String::new();
        opml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        opml.push_str("<opml version=\"1.0\">");
        opml.push_str("<head>");
        opml.push_str("<title>RSS Rust Reader Subscriptions</title>");
        opml.push_str("</head>");
        opml.push_str("<body>");

        // 按分组组织订阅源
        let mut groups: std::collections::BTreeMap<String, Vec<&Feed>> =
            std::collections::BTreeMap::new();

        for feed in &feeds {
            let group_name = feed.group.clone();
            groups.entry(group_name).or_insert_with(Vec::new).push(feed);
        }

        // 输出分组和订阅源
        for (group_name, feeds_in_group) in groups {
            if group_name != "Uncategorized" {
                opml.push_str(&format!(
                    "<outline text=\"{}\" title=\"{}\">\n",
                    Self::escape_xml(&group_name),
                    Self::escape_xml(&group_name)
                ));
            }

            for feed in feeds_in_group {
                opml.push_str(&format!("<outline text=\"{}\" title=\"{}\" type=\"rss\" xmlUrl=\"{}\" htmlUrl=\"{}\" />\n",
                                      Self::escape_xml(&feed.title), Self::escape_xml(&feed.title), 
                                      Self::escape_xml(&feed.url), &feed.link));
            }

            if group_name != "Uncategorized" {
                opml.push_str("</outline>\n");
            }
        }

        opml.push_str("</body></opml>");
        Ok(opml)
    }

    // 辅助函数：转义XML特殊字符
    #[allow(unused)]
    pub fn escape_xml(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace("'", "&apos;")
    }

    /// 导出数据为JSON
    #[allow(unused)]
    pub async fn export_data(&self, export_path: &PathBuf) -> anyhow::Result<()> {
        let feeds = self.get_all_feeds().await?;
        let groups = self.get_all_groups().await?;

        // 获取所有文章
        let conn = self.connection.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles"
        )?;

        let articles = stmt
            .query_map([], |row| {
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date: row.get(5)?,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get::<_, i32>(8)? == 1,
                    is_starred: row.get::<_, i32>(9)? == 1,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            })?
            .collect::<SqliteResult<Vec<Article>>>()?;

        // 构建导出数据结构
        #[derive(Serialize)]
        struct ExportData {
            feeds: Vec<Feed>,
            groups: Vec<FeedGroup>,
            articles: Vec<Article>,
            export_date: DateTime<Utc>,
        }

        let export_data = ExportData {
            feeds,
            groups,
            articles,
            export_date: Utc::now(),
        };

        // 写入文件
        let json = serde_json::to_string_pretty(&export_data)?;
        std::fs::write(export_path, json)?;

        Ok(())
    }

    /// 从JSON导入数据
    #[allow(unused)]
    pub async fn import_data(&mut self, import_path: &PathBuf) -> anyhow::Result<()> {
        let json = std::fs::read_to_string(import_path)?;

        #[derive(Deserialize)]
        struct ImportData {
            feeds: Vec<Feed>,
            groups: Vec<FeedGroup>,
            articles: Vec<Article>,
        }

        let import_data: ImportData = serde_json::from_str(&json)?;
        let mut conn = self.connection.lock().await;
        let tx = conn.transaction()?;

        // 导入分组
        for group in import_data.groups {
            tx.execute(
                "INSERT OR IGNORE INTO feed_groups (name, icon) VALUES (?, ?)",
                params![group.name, group.icon],
            )?;
        }

        // 导入订阅源
        let mut feed_id_map = std::collections::HashMap::new();
        for feed in import_data.feeds {
            // 插入或更新订阅源
            tx.execute(
                "INSERT OR REPLACE INTO feeds 
                 (title, url, group_name, group_id, last_updated, description, language, link, favicon, auto_update, enable_notification, ai_auto_translate) 
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    feed.title,
                    feed.url,
                    feed.group,
                    feed.group_id,
                    feed.last_updated,
                    feed.description,
                    feed.language,
                    feed.link,
                    feed.favicon,
                    feed.auto_update as i8,
                    feed.enable_notification as i8,
                    feed.ai_auto_translate as i8
                ],
            )?;

            // 获取新的ID并建立映射
            let new_id = tx.last_insert_rowid();
            feed_id_map.insert(feed.id, new_id);
        }

        // 导入文章
        for article in import_data.articles {
            // 使用新的feed_id
            let new_feed_id = feed_id_map
                .get(&article.feed_id)
                .copied()
                .unwrap_or(article.feed_id);

            tx.execute(
                "INSERT OR IGNORE INTO articles 
                 (feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid) 
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    new_feed_id,
                    article.title,
                    article.link,
                    article.author,
                    article.pub_date,
                    article.content,
                    article.summary,
                    article.is_read as i32,
                    article.is_starred as i32,
                    article.source,
                    article.guid,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// 辅助函数：移除URL中的锚点部分
    fn remove_url_anchor(url: &str) -> String {
        // 查找#字符的位置
        if let Some(pos) = url.find('#') {
            // 返回锚点前的部分
            url[0..pos].to_string()
        } else {
            // 如果没有锚点，返回原始URL
            url.to_string()
        }
    }

    /// 添加文章

    pub async fn add_articles(
        &mut self,
        feed_id: i64,
        articles: Vec<Article>,
    ) -> anyhow::Result<usize> {
        let mut conn = self.connection.lock().await;
        let tx = conn.transaction()?;
        let mut new_articles_count = 0;

        for article in articles {
            // 移除URL中的锚点部分用于重复检查
            let link_without_anchor = Self::remove_url_anchor(&article.link);

            // 检查是否已存在相同链接(忽略锚点)或相同GUID的文章
            // 优先使用GUID进行精确匹配，然后使用去锚点的链接进行匹配
            // 检查所有订阅源，避免不同源的相同文章重复入库
            let mut stmt = tx.prepare(
                "SELECT id FROM articles WHERE 
                 guid = ? OR link LIKE ?",
            )?;

            // 使用LIKE查询匹配没有锚点或有不同锚点的相同链接
            let link_pattern = format!("{}%", link_without_anchor);

            // 检查是否已存在相同的文章
            let exists = stmt
                .query_row(params![&article.guid, &link_pattern], |row| {
                    row.get::<_, i64>(0)
                })
                .is_ok();

            if !exists {
                // 清理文章内容和摘要
                let cleaned_content = Self::sanitize_html(&article.content);
                let cleaned_summary = Self::sanitize_html(&article.summary);
                let cleaned_title = decode_html_entities(&article.title).to_string();
                
                // 添加新文章，使用INSERT OR IGNORE避免唯一约束错误
                let result = tx.execute(
                    "INSERT OR IGNORE INTO articles (feed_id, title, link, author, pub_date, content, 
                     summary, is_read, is_starred, source, guid) 
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        feed_id,
                        cleaned_title,
                        article.link,
                        article.author,
                        article.pub_date,
                        cleaned_content,
                        cleaned_summary,
                        article.is_read as i32,
                        article.is_starred as i32,
                        article.source,
                        article.guid, // 使用真实的GUID
                    ],
                )?;

                // 如果插入成功，增加计数
                if result > 0 {
                    new_articles_count += 1;
                }
            }
        }

        tx.commit()?;
        Ok(new_articles_count)
    }

    /// 获取指定订阅源的文章

    pub async fn get_articles_by_feed(&self, feed_id: i64) -> anyhow::Result<Vec<Article>> {
        let conn = self.connection.lock().await;

        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles WHERE feed_id = ? ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map(params![feed_id], |row| {
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date: row.get(5)?,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get::<_, i32>(8)? == 1,
                    is_starred: row.get::<_, i32>(9)? == 1,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 更新文章阅读状态
    #[allow(unused)]
    pub async fn update_article_read_status(
        &mut self,
        article_id: i64,
        read: bool,
    ) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        conn.execute(
            "UPDATE articles SET is_read = ? WHERE id = ?",
            params![read as i32, article_id],
        )?;

        Ok(())
    }

    /// 更新文章收藏状态
    #[allow(unused)]
    pub async fn update_article_starred_status(
        &mut self,
        article_id: i64,
        starred: bool,
    ) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        conn.execute(
            "UPDATE articles SET is_starred = ? WHERE id = ?",
            params![starred as i32, article_id],
        )?;

        Ok(())
    }

    /// 获取所有未读文章数量
    #[allow(unused)]
    pub async fn get_unread_count(&self) -> anyhow::Result<u32> {
        let conn = self.connection.lock().await;

        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE is_read = 0",
            [],
            |row| row.get(0),
        )?;

        Ok(count)
    }

    /// 获取指定订阅源的未读文章数量
    #[allow(unused)]
    pub async fn get_feed_unread_count(&self, feed_id: i64) -> anyhow::Result<u32> {
        let conn = self.connection.lock().await;

        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE feed_id = ? AND is_read = 0",
            params![feed_id],
            |row| row.get(0),
        )?;

        Ok(count)
    }

    /// 获取单个文章
    pub async fn get_article(&self, article_id: i64) -> anyhow::Result<Article> {
        let conn = self.connection.lock().await;

        let article = conn.query_row(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles WHERE id = ?",
            params![article_id],
            |row| {
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date: row.get(5)?,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get::<_, i32>(8)? == 1,
                    is_starred: row.get::<_, i32>(9)? == 1,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            },
        )?;

        Ok(article)
    }

    /// 删除指定订阅源的所有文章
    #[allow(unused)]
    pub async fn delete_feed_articles(&mut self, feed_id: i64) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        conn.execute("DELETE FROM articles WHERE feed_id = ?", params![feed_id])?;

        Ok(())
    }

    // delete_article方法已在文件上方定义

    /// 批量标记文章为已读

    pub async fn mark_all_as_read(&mut self, feed_id: Option<i64>) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        match feed_id {
            Some(id) => {
                conn.execute(
                    "UPDATE articles SET is_read = 1 WHERE feed_id = ?",
                    params![id],
                )?;
            }
            None => {
                conn.execute("UPDATE articles SET is_read = 1", [])?;
            }
        }

        Ok(())
    }

    /// 批量标记指定ID的文章为已读
    pub async fn mark_articles_as_read(&mut self, article_ids: &[i64]) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;
        for &id in article_ids {
            conn.execute(
                "UPDATE articles SET is_read = 1 WHERE id = ?",
                params![id],
            )?;
        }
        Ok(())
    }

    /// 删除单篇文章

    pub async fn delete_article(&mut self, article_id: i64) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        conn.execute("DELETE FROM articles WHERE id = ?", params![article_id])?;

        Ok(())
    }

    /// 批量删除文章
    pub async fn delete_all_articles(&mut self, feed_id: Option<i64>) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;

        match feed_id {
            Some(id) => {
                conn.execute("DELETE FROM articles WHERE feed_id = ?", params![id])?;
            }
            None => {
                conn.execute("DELETE FROM articles", [])?;
            }
        }

        Ok(())
    }

    /// 批量删除指定ID的文章
    pub async fn delete_articles(&mut self, article_ids: &[i64]) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;
        for &id in article_ids {
            conn.execute("DELETE FROM articles WHERE id = ?", params![id])?;
        }
        Ok(())
    }
}
