// 数据存储模块

use crate::models::{Feed, Article, FeedGroup};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};

use std::sync::Arc;
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 存储管理器
pub struct StorageManager {
    /// 数据库连接
    connection: Arc<Mutex<Connection>>,
}

impl StorageManager {
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
    let _ = conn.execute("PRAGMA journal_mode = WAL;", []);
    let _ = conn.execute("PRAGMA synchronous = NORMAL;", []);
        
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
                enable_notification INTEGER DEFAULT 1
            )"#,
            [],
        )?;
        
      // 检查并添加auto_update字段（如果不存在）
    let mut stmt = conn.prepare("PRAGMA table_info(feeds)")?;
    let table_info: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?  // 索引1对应列名
        .collect::<SqliteResult<_>>()?;
    
    let has_auto_update = table_info.iter().any(|col| col == "auto_update");
    if !has_auto_update {
        conn.execute(
            "ALTER TABLE feeds ADD COLUMN auto_update INTEGER DEFAULT 1",
            []
        )?;
    }
    
    // 检查并添加enable_notification字段（如果不存在）
    let has_enable_notification = table_info.iter().any(|col| col == "enable_notification");
    if !has_enable_notification {
        conn.execute(
            "ALTER TABLE feeds ADD COLUMN enable_notification INTEGER DEFAULT 1",
            []
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
        let conn = self.connection.lock().await;
        
        // 检查是否已存在相同URL的订阅源
        let mut stmt = conn.prepare("SELECT id FROM feeds WHERE url = ?")?;
        if let Ok(Some(id)) = stmt.query_row(params![&feed.url], |row| row.get(0)).optional() {
            return Ok(id); // 返回已存在的ID
        }
        
        // 添加新订阅源，包含last_updated字段
        let _result = conn.execute(
            "INSERT INTO feeds (title, url, group_name, description, language, link, favicon, last_updated, auto_update, enable_notification) 
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                feed.title,
                feed.url,
                feed.group,
                feed.description,
                feed.language,
                feed.link,
                feed.favicon,
                feed.last_updated.unwrap_or_else(chrono::Utc::now), // 使用当前时间作为默认值
                feed.auto_update as i8,
                feed.enable_notification as i8
            ],
        )?;
        
        // 获取插入的ID
        let id = conn.last_insert_rowid();
        
        Ok(id)
    }
    
    /// 获取所有订阅源
    
    pub async fn get_all_feeds(&self) -> anyhow::Result<Vec<Feed>> {
        let conn = self.connection.lock().await;
        
        let mut stmt = conn.prepare(
            "SELECT id, title, url, group_name, last_updated, description, language, link, favicon, auto_update, enable_notification 
             FROM feeds ORDER BY group_name, title"
        )?;
        
        let feeds = stmt.query_map([], |row| {
            Ok(Feed {
                id: row.get(0)?,
                title: row.get(1)?,
                url: row.get(2)?,
                group: row.get(3)?,
                last_updated: row.get(4)?,
                description: row.get(5)?,
                language: row.get(6)?,
                link: row.get(7)?,
                favicon: row.get(8)?,
                auto_update: row.get(9)?,
                enable_notification: row.get(10)?,
            })
        })?
        .collect::<SqliteResult<Vec<Feed>>>()?;
        
        Ok(feeds)
    }
    
    /// 更新订阅源信息
    
    pub async fn update_feed(&mut self, feed: &Feed) -> anyhow::Result<()>
    {
        let conn = self.connection.lock().await;
        
        conn.execute(
            "UPDATE feeds SET title = ?, url = ?, group_name = ?, description = ?, 
             language = ?, link = ?, favicon = ?, auto_update = ?, enable_notification = ? WHERE id = ?",
            params![
                feed.title,
                feed.url,
                feed.group,
                feed.description,
                feed.language,
                feed.link,
                feed.favicon,
                feed.auto_update as i8,
                feed.enable_notification as i8,
                feed.id
            ],
        )?;
        
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
        
        let mut stmt = conn.prepare(
            "SELECT id, name, icon FROM feed_groups ORDER BY name"
        )?;
        
        let groups = stmt.query_map([], |row| {
            Ok(FeedGroup {
                id: row.get(0)?,
                name: row.get(1)?,
                icon: row.get(2)?,
            })
        })?
        .collect::<SqliteResult<Vec<FeedGroup>>>()?;
        
        Ok(groups)
    }
    
    /// 删除分组
    #[allow(unused)]
    pub async fn delete_group(&mut self, group_id: i64) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;
        
        // 首先获取要删除的分组名称
        let group_name: String = conn.query_row(
            "SELECT name FROM feed_groups WHERE id = ?",
            params![group_id],
            |row| row.get(0)
        )?;
        
        // 先将使用此分组的订阅源的分组名设为空
        conn.execute("UPDATE feeds SET group_name = '' WHERE group_name = ?", params![group_name])?;
        
        // 删除分组
        conn.execute("DELETE FROM feed_groups WHERE id = ?", params![group_id])?;
        
        Ok(())
    }
    
    /// 更新分组名称
    #[allow(unused)]
    pub async fn update_group_name(&mut self, old_name: &str, new_name: &str) -> anyhow::Result<()> {
        let mut conn = self.connection.lock().await;
        
        // 验证新名称不为空
        if new_name.trim().is_empty() {
            return Err(anyhow::anyhow!("分组名称不能为空"));
        }
        
        // 开始事务
        let tx = conn.transaction()?;
        
        // 更新feed_groups表中的分组名称
        tx.execute(
            "UPDATE feed_groups SET name = ? WHERE name = ?",
            params![new_name.trim(), old_name]
        )?;
        
        // 更新所有使用该分组的订阅源的group_name字段
        tx.execute(
            "UPDATE feeds SET group_name = ? WHERE group_name = ?",
            params![new_name.trim(), old_name]
        )?;
        
        // 提交事务
        tx.commit()?;
        
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
    pub async fn get_setting<T: for<'a> Deserialize<'a>>(&self, key: &str) -> anyhow::Result<Option<T>> {
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
        
        let articles = stmt.query_map([], |row| {
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
                guid: row.get::<_, String>(11).unwrap_or_else(|_| row.get::<_, String>(3).unwrap_or_default())
            })
        })?
        .collect::<SqliteResult<Vec<Article>>>()?;

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
        
        let articles = stmt.query_map([], |row| {
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
        
        let articles = stmt.query_map(params![&search_pattern, &search_pattern, &search_pattern], |row| {
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
        
        Ok(articles)
    }
    
    /// 清理旧文章（保留最近N篇）
    #[allow(unused)]
    pub async fn cleanup_old_articles(&mut self, feed_id: i64, keep_count: u32) -> anyhow::Result<()> {
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
        let mut groups: std::collections::BTreeMap<String, Vec<&Feed>> = std::collections::BTreeMap::new();
        
        for feed in &feeds {
            let group_name = feed.group.clone();
            groups.entry(group_name).or_insert_with(Vec::new).push(feed);
        }
        
        // 输出分组和订阅源
        for (group_name, feeds_in_group) in groups {
            if group_name != "Uncategorized" {
                opml.push_str(&format!("<outline text=\"{}\" title=\"{}\">\n", 
                                      Self::escape_xml(&group_name), Self::escape_xml(&group_name)));
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
        
        let articles = stmt.query_map([], |row| {
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
                 (title, url, group_name, last_updated, description, language, link, favicon) 
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    feed.title,
                    feed.url,
                    feed.group,
                    feed.last_updated,
                    feed.description,
                    feed.language,
                    feed.link,
                    feed.favicon
                ],
            )?;
            
            // 获取新的ID并建立映射
            let new_id = tx.last_insert_rowid();
            feed_id_map.insert(feed.id, new_id);
        }
        
        // 导入文章
        for article in import_data.articles {
            // 使用新的feed_id
            let new_feed_id = feed_id_map.get(&article.feed_id).copied().unwrap_or(article.feed_id);
            
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
    
    pub async fn add_articles(&mut self, feed_id: i64, articles: Vec<Article>) -> anyhow::Result<usize> {
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
                 guid = ? OR link LIKE ?"
            )?;
            
            // 使用LIKE查询匹配没有锚点或有不同锚点的相同链接
            let link_pattern = format!("{}%", link_without_anchor);
            
            // 检查是否已存在相同的文章
            let exists = stmt.query_row(
                params![&article.guid, &link_pattern], 
                |row| row.get::<_, i64>(0)
            ).is_ok();
            
            if !exists {
                // 添加新文章，使用INSERT OR IGNORE避免唯一约束错误
                let result = tx.execute(
                    "INSERT OR IGNORE INTO articles (feed_id, title, link, author, pub_date, content, 
                     summary, is_read, is_starred, source, guid) 
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        feed_id,
                        article.title,
                        article.link,
                        article.author,
                        article.pub_date,
                        article.content,
                        article.summary,
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
        
        let articles = stmt.query_map(params![feed_id], |row| {
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
        
        Ok(articles)
    }
    
    /// 更新文章阅读状态
    #[allow(unused)]
    pub async fn update_article_read_status(&mut self, article_id: i64, read: bool) -> anyhow::Result<()> {
        let conn = self.connection.lock().await;
        
        conn.execute(
            "UPDATE articles SET is_read = ? WHERE id = ?",
            params![read as i32, article_id],
        )?;
        
        Ok(())
    }
    
    /// 更新文章收藏状态
    #[allow(unused)]
    pub async fn update_article_starred_status(&mut self, article_id: i64, starred: bool) -> anyhow::Result<()> {
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
                conn.execute("UPDATE articles SET is_read = 1 WHERE feed_id = ?", params![id])?;
            },
            None => {
                conn.execute("UPDATE articles SET is_read = 1", [])?;
            },
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
            },
            None => {
                conn.execute("DELETE FROM articles", [])?;
            },
        }
        
        Ok(())
    }
}
