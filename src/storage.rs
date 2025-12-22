// 数据存储模块

use crate::models::{Article, Feed, FeedGroup};
use chrono::{DateTime, Utc};
use duckdb::{Connection, OptionalExt, Result as DuckDBResult, params};
use duckdb::types::ValueRef;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use ammonia::{Builder, UrlRelative};
use html_escape::decode_html_entities;

/// 存储管理器
pub struct StorageManager {

    /// 共享数据库连接
    conn: Mutex<Connection>,
}

impl StorageManager {
    /// 清理HTML内容，保留主要文本内容和基本结构
    pub fn sanitize_html(html: &str) -> String {
        // 首先检查输入是否为空
        if html.is_empty() {
            return String::new();
        }
        
        // 尝试处理HTML内容，添加错误处理
        match std::panic::catch_unwind(|| {
            // 创建ammonia构建器
            let mut builder = Builder::new();
            
            // 允许更多常用标签
            let allowed_tags = HashSet::from(["p", "div", "span", "h1", "h2", "h3", "h4", "h5", "h6", 
                                             "br", "hr", "ul", "ol", "li", "strong", "em", "b", "i",
                                             "code", "pre", "blockquote"]);
            builder.tags(allowed_tags);
            
            // 允许基本属性
            let mut tag_attributes = HashMap::new();
            // 允许所有标签的class属性（用于样式）
            tag_attributes.insert("*", HashSet::from(["class"]));
            // 允许链接和图片属性
            tag_attributes.insert("a", HashSet::from(["href", "title"]));
            tag_attributes.insert("img", HashSet::from(["src", "alt", "title"]));
            builder.tag_attributes(tag_attributes);
            
            // 设置URL相对路径处理
            builder.url_relative(UrlRelative::PassThrough);
            
            // 设置链接rel属性
            builder.link_rel(Some("noopener noreferrer"));
            
            // 清理HTML内容
            let sanitized_html = builder.clean(html).to_string();
            
            // 解码HTML实体
            let decoded = decode_html_entities(&sanitized_html);
            
            // 尝试提取所有文本内容，保留基本结构
            // 首先使用更简单的方式处理，避免使用不支持的反向引用
            let re = regex::Regex::new(r"<[^>]*>")
                .expect("正则表达式创建失败");
            
            // 移除所有HTML标签，只保留文本内容
            let simple_text = re.replace_all(&decoded, "").to_string();
            
            // 处理文本，将多个空格替换为一个，保留换行符
            let mut result = String::new();
            let mut last_char = ' ';
            
            for c in simple_text.chars() {
                if c.is_whitespace() {
                    if c == '\n' || c == '\r' {
                        if last_char != '\n' {
                            result.push('\n');
                            last_char = '\n';
                        }
                    } else if !last_char.is_whitespace() {
                        result.push(' ');
                        last_char = ' ';
                    }
                } else {
                    result.push(c);
                    last_char = c;
                }
            }
            
            // 移除首尾空白
            let trimmed = result.trim();
            
            // 将文本按段落分割
            let paragraphs: Vec<String> = trimmed
                .split_inclusive("\n\n")
                .map(|s| s.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            
            // 使用回车换行符连接段落
            paragraphs.join("\n\n")
        }) {
            Ok(cleaned_content) => {
                // 如果主要逻辑返回空，使用备用处理方式
                if cleaned_content.is_empty() {
                    // 直接使用更简单的方式处理
                    log::warn!("HTML清理主要逻辑返回空，使用备用处理方式");
                    // 解码HTML实体
                    let decoded = decode_html_entities(html).to_string();
                    // 移除HTML标签
                    let re = match regex::Regex::new(r"<[^>]*>") {
                        Ok(re) => re,
                        Err(_) => {
                            // 如果正则表达式创建失败，直接返回解码后的内容
                            return decoded;
                        }
                    };
                    let simple_cleaned = re.replace_all(&decoded, "").to_string();
                    // 处理换行符，将多个空格替换为一个，保留换行
                    let mut result = String::new();
                    let mut last_char = ' ';
                    
                    for c in simple_cleaned.chars() {
                        if c.is_whitespace() {
                            if c == '\n' || c == '\r' {
                                if last_char != '\n' {
                                    result.push('\n');
                                    last_char = '\n';
                                }
                            } else if !last_char.is_whitespace() {
                                result.push(' ');
                                last_char = ' ';
                            }
                        } else {
                            result.push(c);
                            last_char = c;
                        }
                    }
                    
                    // 移除首尾空白
                    result.trim().to_string()
                } else {
                    cleaned_content
                }
            },
            Err(_) => {
                // 如果处理失败，使用更简单的方式处理
                log::warn!("HTML清理失败，使用简单处理方式");
                // 解码HTML实体
                let decoded = decode_html_entities(html).to_string();
                // 移除HTML标签
                let re = match regex::Regex::new(r"<[^>]*>") {
                    Ok(re) => re,
                    Err(_) => {
                        // 如果正则表达式创建失败，直接返回解码后的内容
                        return decoded;
                    }
                };
                let simple_cleaned = re.replace_all(&decoded, "").to_string();
                // 处理换行符，将多个空格替换为一个，保留换行
                let mut result = String::new();
                let mut last_char = ' ';
                
                for c in simple_cleaned.chars() {
                    if c.is_whitespace() {
                        if c == '\n' || c == '\r' {
                            if last_char != '\n' {
                                result.push('\n');
                                last_char = '\n';
                            }
                        } else if !last_char.is_whitespace() {
                            result.push(' ');
                            last_char = ' ';
                        }
                    } else {
                        result.push(c);
                        last_char = c;
                    }
                }
                
                // 移除首尾空白
                result.trim().to_string()
            }
        }
    }

    /// 创建新的存储管理器
    pub fn new(db_path: String) -> Self {
        // 确保数据库目录存在
        if let Some(parent) = PathBuf::from(&db_path).parent()
            && let Err(create_err) = std::fs::create_dir_all(parent) {
            eprintln!("错误: 无法创建数据库目录: {}", create_err);
        }

        // 初始化数据库连接
        let conn = Connection::open(&db_path).expect("创建数据库连接失败");
        
        // 初始化数据库表
        match Self::init_database(&conn) {
            Ok(_) => (),
            Err(e) => {
                eprintln!("警告: 数据库表初始化时出现警告: {}", e);
                // 即使初始化有警告也继续运行，尽量恢复
            }
        }

        // DuckDB不需要SQLite特定的PRAGMA设置
        // DuckDB会自动管理存储空间，不需要手动执行VACUUM
        // 移除VACUUM命令以避免资源死锁问题

        Self {
           
            conn: Mutex::new(conn),
        }
    }



    /// 初始化数据库表
    fn init_database(conn: &duckdb::Connection) -> DuckDBResult<()> {
        // 创建订阅源分组表的序列
        conn.execute(
            "CREATE SEQUENCE IF NOT EXISTS feed_groups_id_seq START 1;",
            [],
        )?;

        // 创建订阅源分组表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS feed_groups (
                id BIGINT PRIMARY KEY DEFAULT nextval('feed_groups_id_seq'),
                name TEXT NOT NULL UNIQUE,
                icon TEXT
            )"#,
            [],
        )?;

        // 创建订阅源表的序列
        conn.execute(
            "CREATE SEQUENCE IF NOT EXISTS feeds_id_seq START 1;",
            [],
        )?;

        // 创建订阅源表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS feeds (
                id BIGINT PRIMARY KEY DEFAULT nextval('feeds_id_seq'),
                title TEXT NOT NULL,
                url TEXT NOT NULL UNIQUE,
                group_id BIGINT DEFAULT NULL,
                last_updated TIMESTAMP,
                description TEXT,
                language TEXT,
                link TEXT,
                favicon TEXT,
                auto_update BOOLEAN DEFAULT TRUE,
                enable_notification BOOLEAN DEFAULT TRUE,
                ai_auto_translate BOOLEAN DEFAULT FALSE
            )"#,
            [],
        )?;

        // 检查并添加auto_update字段（如果不存在）
        let mut stmt = conn.prepare("SELECT column_name FROM information_schema.columns WHERE table_name = 'feeds'")?;
        let table_info: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))? // 索引0对应column_name
            .collect::<DuckDBResult<_>>()?;

        let has_auto_update = table_info.iter().any(|col| col == "auto_update");
        if !has_auto_update {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN auto_update BOOLEAN DEFAULT TRUE",
                [],
            )?;
        }

        // 检查并添加enable_notification字段（如果不存在）
        let has_enable_notification = table_info.iter().any(|col| col == "enable_notification");
        if !has_enable_notification {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN enable_notification BOOLEAN DEFAULT TRUE",
                [],
            )?;
        }

        // 检查并添加ai_auto_translate字段（如果不存在）
        let has_ai_auto_translate = table_info.iter().any(|col| col == "ai_auto_translate");
        if !has_ai_auto_translate {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN ai_auto_translate BOOLEAN DEFAULT TRUE",
                [],
            )?;
        }

        // 检查并添加group_id字段（如果不存在）
        let has_group_id = table_info.iter().any(|col| col == "group_id");
        if !has_group_id {
            conn.execute(
                "ALTER TABLE feeds ADD COLUMN group_id BIGINT DEFAULT NULL",
                [],
            )?;
        } else {
            // 如果group_id字段存在，检查并修改其类型为BIGINT
            // DuckDB不支持直接修改列类型，所以我们需要重新创建表
            // 但为了简化，我们先跳过这个检查，因为DuckDB会自动处理类型转换
        }

        // 创建文章表的序列
        conn.execute(
            "CREATE SEQUENCE IF NOT EXISTS articles_id_seq START 1;",
            [],
        )?;

        // 创建文章表
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS articles (
                id BIGINT PRIMARY KEY DEFAULT nextval('articles_id_seq'),
                feed_id BIGINT NOT NULL,
                title TEXT NOT NULL,
                link TEXT,
                author TEXT,
                pub_date TIMESTAMP NOT NULL,
                content TEXT,
                summary TEXT,
                is_read BOOLEAN DEFAULT FALSE,
                is_starred BOOLEAN DEFAULT FALSE,
                source TEXT,
                guid TEXT
            )"#,
            [],
        )?;

        // 创建索引以提高性能
        // 优化：使用组合索引替代多个单字段索引，减少索引数量和维护开销
        // 组合索引：feed_id + pub_date + is_read + is_starred，覆盖常用查询条件
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_feed_date_status ON articles(feed_id, pub_date DESC, is_read, is_starred)",
            [],
        )?;
        
        // 保留单独的is_starred索引，用于快速查询收藏文章
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_starred ON articles(is_starred, pub_date DESC)",
            [],
        )?;
        
        // 保留单独的is_read索引，用于快速查询未读文章
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_read ON articles(is_read, pub_date DESC)",
            [],
        )?;
        
        // 处理重复的guid值，避免创建唯一索引时失败
        // 1. 首先创建普通索引，不使用唯一约束
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_guid_temp ON articles(guid)",
            [],
        )?;
        
        // 2. 清理表中重复的guid值，保留id最小的记录
        conn.execute(
            r#"DELETE FROM articles 
               WHERE id NOT IN (
                   SELECT MIN(id) FROM articles 
                   GROUP BY guid 
                   HAVING guid IS NOT NULL AND guid != ''
               ) AND guid IS NOT NULL AND guid != ''"#,
            [],
        )?;
        
        // 3. 删除临时索引
        conn.execute(
            "DROP INDEX IF EXISTS idx_articles_guid_temp",
            [],
        )?;
        
        // 4. 现在可以安全地创建唯一索引了
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_articles_guid ON articles(guid)",
            [],
        )?;
        
        // 添加索引以提高重复检查的性能
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_articles_link ON articles(link)",
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
        log::info!("开始添加订阅源，URL: {}", feed.url);
        
        // 获取数据库连接
        let mut conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        log::info!("获取数据库连接成功");
        
        // 开始事务，将所有操作放在一个事务中
        let tx = conn.transaction()?;
        log::info!("创建事务成功");
        
        // 检查是否已存在相同URL的订阅源
        log::info!("检查是否已存在相同URL的订阅源");
        let select_sql = "SELECT id FROM feeds WHERE url = ?";
        log::info!("执行SQL: {}", select_sql);
        
        let mut stmt = tx.prepare(select_sql)?;
        log::info!("准备SELECT语句成功");
        
        let query_result = stmt.query_row(params![&feed.url], |row: &duckdb::Row| row.get(0)).optional()?;
        
        if let Some(id) = query_result {
            log::info!("订阅源已存在，返回ID: {}", id);
            tx.commit()?;
            return Ok(id); // 返回已存在的ID
        }
        log::info!("订阅源不存在，准备添加");
        
        // 处理分组逻辑
        let group_id = if feed.group.is_empty() {
            // 空分组，使用NULL
            log::info!("订阅源分组为空，使用NULL");
            None
        } else {
            log::info!("处理订阅源分组: {}", feed.group);
            // 直接查询分组ID，不使用UPSERT，避免复杂逻辑
            let select_group_sql = "SELECT id FROM feed_groups WHERE name = ?";
            log::info!("执行分组查询SQL: {}", select_group_sql);
            
            let mut group_stmt = tx.prepare(select_group_sql)?;
            let group_query_result = group_stmt.query_row(params![&feed.group], |row: &duckdb::Row| row.get::<_, i64>(0)).optional()?;
            
            if let Some(id) = group_query_result {
                log::info!("分组已存在，获取到分组ID: {}", id);
                Some(id)
            } else {
                log::info!("分组不存在，准备插入新分组");
                // 插入新分组
                let insert_group_sql = "INSERT INTO feed_groups (name, icon) VALUES (?, ?)";
                log::info!("执行插入分组SQL: {}", insert_group_sql);
                
                tx.execute(insert_group_sql, params![feed.group, None::<Option<&str>>])?;
                
                log::info!("插入分组成功，准备获取新分组ID");
                // 获取插入的ID
                let new_group_id = tx.query_row(
                    select_group_sql,
                    params![&feed.group],
                    |row: &duckdb::Row| row.get::<_, i64>(0)
                )?;
                
                log::info!("获取到新分组ID: {}", new_group_id);
                Some(new_group_id)
            }
        };

        // 添加新订阅源，包含last_updated字段
        let last_updated = feed.last_updated.unwrap_or_else(chrono::Utc::now);
        log::info!("准备插入新订阅源，last_updated: {:?}", last_updated);
        
        // 插入新订阅源，直接包含group_id字段
        let insert_sql = "INSERT INTO feeds (title, url, group_id, last_updated, description, language, link, favicon, auto_update, enable_notification, ai_auto_translate) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id";
        log::info!("执行插入订阅源SQL: {}", insert_sql);
        log::info!("插入参数: title={}, url={}, group_id={:?}", feed.title, feed.url, group_id);
        
        let id = tx.query_row(
            insert_sql,
            params![
                feed.title,
                feed.url,
                group_id,
                last_updated,
                feed.description,
                feed.language,
                feed.link,
                feed.favicon,
                feed.auto_update,
                feed.enable_notification,
                feed.ai_auto_translate
            ],
            |row: &duckdb::Row| row.get::<_, i64>(0)
        )?;
        
        log::info!("插入订阅源成功，返回ID: {}", id);
        
        // 提交事务
        log::info!("提交事务");
        tx.commit()?;
        
        log::info!("添加订阅源成功，返回ID: {}", id);

        Ok(id)
    }

    /// 执行数据库VACUUM操作，清理过期数据版本，回收磁盘空间
    #[allow(unused)]
    pub async fn vacuum_database(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        
        // 执行VACUUM命令
        conn.execute("VACUUM", [])?;
        
        Ok(())
    }

    /// 清理旧文章，根据配置删除超过保留天数或超过每个订阅源最大文章数的旧文章
    #[allow(unused)]
    pub async fn cleanup_old_articles(&self, retention_days: u32, max_articles_per_feed: u32) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        let tx = conn.transaction()?;
        
        // 计算清理日期
        let cutoff_date = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
        
        // 清理超过保留天数的文章，保留收藏的文章
        let deleted_by_date = tx.execute(
            "DELETE FROM articles WHERE pub_date < ? AND is_starred = 0",
            params![cutoff_date]
        )?;
        
        // 清理每个订阅源超过最大数量的旧文章，保留收藏的文章和未过期的文章
        // 先获取每个订阅源需要保留的文章ID
        let deleted_by_count = tx.execute(
            r#"DELETE FROM articles 
               WHERE id NOT IN (
                   SELECT id FROM articles 
                   WHERE is_starred = 1 OR pub_date >= ?
               ) 
               AND id NOT IN (
                   SELECT id FROM (
                       SELECT id, ROW_NUMBER() OVER (PARTITION BY feed_id ORDER BY pub_date DESC) as rn
                       FROM articles 
                       WHERE is_starred = 0 AND pub_date < ?
                   ) t
                   WHERE t.rn <= ?
               )"#, 
            params![cutoff_date, cutoff_date, max_articles_per_feed]
        )?;
        
        tx.commit()?;
        
        Ok(deleted_by_date + deleted_by_count)
    }

    /// 获取所有订阅源
    pub async fn get_all_feeds(&self) -> anyhow::Result<Vec<Feed>> {
        // 获取新的数据库连接
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        // 使用更简单的查询，避免复杂的JOIN和COALESCE
        // 直接获取订阅源和分组信息，DuckDB应该能正确处理
        let mut stmt = conn.prepare(
            "SELECT f.id, f.title, f.url, f.group_id, g.name, f.last_updated, f.description, f.language, f.link, f.favicon, f.auto_update, f.enable_notification, f.ai_auto_translate 
             FROM feeds f LEFT JOIN feed_groups g ON f.group_id = g.id ORDER BY f.title"
        )?;

        let feeds = stmt
            .query_map([], |row| {
                // 获取last_updated字段，处理NULL值并从字符串解析为DateTime<Utc>
                let last_updated_str: Option<String> = row.get(5)?;
                let last_updated = match last_updated_str {
                    Some(s) => {
                        // 尝试多种时间格式解析
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s) {
                            Some(dt.with_timezone(&chrono::Utc))
                        } else if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&s) {
                            Some(dt.with_timezone(&chrono::Utc))
                        } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S.%f") {
                            // 支持ISO格式：2025-12-16 03:45:35.063049
                            Some(chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc))
                        } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S") {
                            // 支持ISO格式：2025-12-16 03:45:35
                            Some(chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc))
                        } else {
                            // 所有解析尝试都失败，使用当前时间作为默认值
                            log::warn!("无法解析last_updated: {}, 使用当前时间作为默认值", s);
                            Some(chrono::Utc::now())
                        }
                    },
                    None => None,
                };
                
                Ok(Feed {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    url: row.get(2)?,
                    group: row.get(4).unwrap_or_default(),
                    group_id: row.get(3)?,
                    last_updated,
                    description: row.get(6)?,
                    language: row.get(7)?,
                    link: row.get(8)?,
                    favicon: row.get(9)?,
                    auto_update: row.get(10)?,
                    enable_notification: row.get(11)?,
                    ai_auto_translate: row.get(12)?,
                })
            })?
            .collect::<DuckDBResult<Vec<Feed>>>()?;

        Ok(feeds)
    }

  
    

    /// 更新订阅源信息
    pub async fn update_feed(&mut self, feed: &Feed) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        
        // 处理分组逻辑
        let group_id = if feed.group.is_empty() {
            // 空分组，使用NULL
            None
        } else {
            // 查询是否已存在同名分组
            let mut group_stmt = conn.prepare("SELECT id FROM feed_groups WHERE name = ?")?;
            let group_exists = group_stmt
                .query_row(params![&feed.group], |row| row.get::<_, i64>(0))
                .optional()?;
            
            if let Some(id) = group_exists {
                Some(id)
            } else {
                // 不存在，插入新分组
                conn.execute(
                    "INSERT INTO feed_groups (name, icon) VALUES (?, ?)",
                    params![feed.group, None::<&str>],
                )?;
                
                // 获取插入的ID
                let new_group_id = conn.query_row(
                    "SELECT id FROM feed_groups WHERE name = ?",
                    params![&feed.group],
                    |row| row.get::<_, i64>(0),
                )?;
                
                Some(new_group_id)
            }
        };

        conn.execute(
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
                feed.auto_update,
                feed.enable_notification,
                feed.ai_auto_translate,
                feed.id
            ],
        )?;

        Ok(())
    }

    /// 更新订阅源最后更新时间
    pub async fn update_feed_last_updated(&mut self, feed_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        // 直接使用chrono::DateTime类型，DuckDB驱动支持chrono特性
        let now = Utc::now();
        conn.execute(
            "UPDATE feeds SET last_updated = ? WHERE id = ?",
            params![now, feed_id],
        )?;

        Ok(())
    }

    /// 删除订阅源
    pub async fn delete_feed(&mut self, feed_id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        // 先删除相关的文章
        conn.execute("DELETE FROM articles WHERE feed_id = ?", params![feed_id])?;
        
        // 再删除订阅源
        conn.execute("DELETE FROM feeds WHERE id = ?", params![feed_id])?;

        Ok(())
    }

    /// 获取所有订阅源分组
    pub async fn get_all_groups(&self) -> anyhow::Result<Vec<FeedGroup>> {
          let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        let mut stmt = conn.prepare("SELECT id, name, icon FROM feed_groups ORDER BY name")?;

        let groups = stmt
            .query_map([], |row| {
                Ok(FeedGroup {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    icon: row.get(2)?,
                })
            })?
            .collect::<DuckDBResult<Vec<FeedGroup>>>()?;

        Ok(groups)
    }

    /// 删除分组
    #[allow(unused)]
    pub async fn delete_group(&mut self, group_id: i64) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();

        // 开始事务
        let tx = conn.transaction()?;
        
        // 先将使用此分组的订阅源的group_id设为NULL
        tx.execute(
            "UPDATE feeds SET group_id = NULL WHERE group_id = ?",
            params![group_id],
        )?;

        // 删除分组
        tx.execute("DELETE FROM feed_groups WHERE id = ?", params![group_id])?;
        
        // 提交事务
        tx.commit()?;

        Ok(())
    }

    /// 添加分组
    #[allow(unused)]
    pub async fn add_group(&mut self, group_name: &str, icon: Option<&str>) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        // 验证分组名称不为空
        if group_name.trim().is_empty() {
            return Err(anyhow::anyhow!("分组名称不能为空"));
        }

        let trimmed_name = group_name.trim();
        
        // 先检查分组是否已存在
        let group_exists = conn
            .query_row(
                "SELECT id FROM feed_groups WHERE name = ?",
                params![trimmed_name],
                |row| row.get::<_, i64>(0)
            )
            .optional()?;
        
        if let Some(id) = group_exists {
            // 分组已存在，直接返回ID
            Ok(id)
        } else {
            // 分组不存在，插入新分组
            // 使用DuckDB兼容的INSERT语法，不使用INSERT OR IGNORE
            conn.execute(
                "INSERT INTO feed_groups (name, icon) VALUES (?, ?)",
                params![trimmed_name, icon],
            )?;

            // 获取插入的ID
            let group_id = conn.query_row(
                "SELECT id FROM feed_groups WHERE name = ?",
                params![trimmed_name],
                |row| row.get(0),
            )?;

            Ok(group_id)
        }
    }

    /// 更新分组名称
    #[allow(unused)]
    pub async fn update_group_name(
        &mut self,
        group_id: i64,
        new_name: &str,
    ) -> anyhow::Result<()> {
        log::debug!("[update_group_name] 开始执行，group_id: {}, new_name: '{}'", group_id, new_name);
        
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        
        // 检查新名称是否已被其他分组使用
        let mut name_check_stmt = conn.prepare("SELECT id FROM feed_groups WHERE name = ? AND id != ?")?;
        let name_exists = name_check_stmt
            .query_row(params![new_name, group_id], |row| row.get::<_, i64>(0))
            .optional()?;
        
        if name_exists.is_some() {
            return Err(anyhow::anyhow!("分组名称 '{}' 已存在", new_name));
        }
        
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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        
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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        let json_value = serde_json::to_string(value)?;

        // 使用DuckDB原生的INSERT ON CONFLICT语法
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

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
        if !(1..=1440).contains(&interval) {
            anyhow::bail!("更新间隔必须在1-1440分钟之间");
        }

        self.save_setting(SETTING_KEY, &interval).await
    }

    /// 获取所有收藏的文章
    #[allow(unused)]
    pub async fn get_starred_articles(&self) -> anyhow::Result<Vec<Article>> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles WHERE is_starred = TRUE ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map([], |row| {
                // 获取pub_date字段，从字符串解析为DateTime<Utc>
                let pub_date_str: String = row.get(5)?;
                let pub_date = match chrono::DateTime::parse_from_rfc3339(&pub_date_str) {
                    Ok(dt) => dt.with_timezone(&chrono::Utc),
                    Err(e) => return Err(duckdb::Error::ToSqlConversionFailure(Box::new(e))),
                };
                
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get(8)?,
                    is_starred: row.get(9)?,
                    source: row.get(10)?,
                    guid: row
                        .get::<_, String>(11)
                        .unwrap_or_else(|_| row.get::<_, String>(3).unwrap_or_default()),
                })
            })?
            .collect::<DuckDBResult<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 获取所有文章
    #[allow(unused)]
    pub async fn get_all_articles(&self) -> anyhow::Result<Vec<Article>> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map([], |row| {
                // 获取pub_date字段，从字符串解析为DateTime<Utc>，增强容错性
                let pub_date = match row.get::<_, Option<String>>(5)? {
                    Some(pub_date_str) => {
                        // 尝试多种时间格式解析
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&pub_date_str) {
                            dt.with_timezone(&chrono::Utc)
                        } else if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&pub_date_str) {
                            dt.with_timezone(&chrono::Utc)
                        } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&pub_date_str, "%Y-%m-%d %H:%M:%S.%f") {
                            // 支持ISO格式：2025-12-16 03:45:35.063049
                            chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                        } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&pub_date_str, "%Y-%m-%d %H:%M:%S") {
                            // 支持ISO格式：2025-12-16 03:45:35
                            chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                        } else {
                            // 所有解析尝试都失败，使用当前时间作为默认值
                            log::warn!("无法解析pub_date: {}, 使用当前时间作为默认值", pub_date_str);
                            chrono::Utc::now()
                        }
                    },
                    None => {
                        // 数据库中为NULL，使用当前时间作为默认值
                        log::warn!("pub_date为NULL，使用当前时间作为默认值");
                        chrono::Utc::now()
                    }
                };
                
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get(8)?,
                    is_starred: row.get(9)?,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            })?
            .collect::<DuckDBResult<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 搜索文章
    #[allow(unused)]
    pub async fn search_articles(&self, query: &str) -> anyhow::Result<Vec<Article>> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

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
                    // 获取pub_date字段，从字符串解析为DateTime<Utc>
                    let pub_date_str: String = row.get(5)?;
                    let pub_date = match chrono::DateTime::parse_from_rfc3339(&pub_date_str) {
                        Ok(dt) => dt.with_timezone(&chrono::Utc),
                        Err(e) => return Err(duckdb::Error::ToSqlConversionFailure(Box::new(e))),
                    };
                    
                    Ok(Article {
                        id: row.get(0)?,
                        feed_id: row.get(1)?,
                        title: row.get(2)?,
                        link: row.get(3)?,
                        author: row.get(4)?,
                        pub_date,
                        content: row.get(6)?,
                        summary: row.get(7)?,
                        is_read: row.get(8)?,
                        is_starred: row.get(9)?,
                        source: row.get(10)?,
                        guid: row.get(11)?,
                    })
                },
            )?
            .collect::<DuckDBResult<Vec<Article>>>()?;

        Ok(articles)
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
        // 优化：使用&str作为键，避免不必要的clone操作
        let mut groups: std::collections::HashMap<&str, Vec<&Feed>> = 
            std::collections::HashMap::new();

        for feed in &feeds {
            // 直接使用feed.group的引用作为键，无需clone
            let group_name = &feed.group[..];
            groups.entry(group_name).or_default().push(feed);
        }

        // 将分组转换为BTreeMap以保持排序
        let mut sorted_groups: std::collections::BTreeMap<&str, Vec<&Feed>> = 
            groups.into_iter().collect();

        // 输出分组和订阅源
        for (group_name, feeds_in_group) in sorted_groups {
            if group_name != "Uncategorized" {
                opml.push_str(&format!(
                    "<outline text=\"{}\" title=\"{}\">\n",
                    Self::escape_xml(group_name),
                    Self::escape_xml(group_name)
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
    pub async fn export_data(&self, export_path: &PathBuf) -> anyhow::Result<()>
    {
        let feeds = self.get_all_feeds().await?;
        let groups = self.get_all_groups().await?;

        // 获取所有文章
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles"
        )?;

        let articles = stmt
            .query_map([], |row| {
                // 获取pub_date字段，从字符串解析为DateTime<Utc>
                let pub_date_str: String = row.get(5)?;
                let pub_date = match chrono::DateTime::parse_from_rfc3339(&pub_date_str) {
                    Ok(dt) => dt.with_timezone(&chrono::Utc),
                    Err(e) => return Err(duckdb::Error::ToSqlConversionFailure(Box::new(e))),
                };
                
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get::<_, i32>(8)? == 1,
                    is_starred: row.get::<_, i32>(9)? == 1,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            })?
            .collect::<DuckDBResult<Vec<Article>>>()?;

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
    pub async fn import_data(&mut self, import_path: &PathBuf) -> anyhow::Result<()>
    {
        let json = std::fs::read_to_string(import_path)?;

        #[derive(Deserialize)]
        struct ImportData {
            feeds: Vec<Feed>,
            groups: Vec<FeedGroup>,
            articles: Vec<Article>,
        }

        let import_data: ImportData = serde_json::from_str(&json)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        // 导入分组
        for group in import_data.groups {
            // 先检查分组是否已存在
            let group_exists = tx
                .query_row(
                    "SELECT id FROM feed_groups WHERE name = ?",
                    params![group.name],
                    |row| row.get::<_, i64>(0)
                )
                .optional()?;
            
            if group_exists.is_none() {
                // 分组不存在，插入新分组
                tx.execute(
                    "INSERT INTO feed_groups (name, icon) VALUES (?, ?)",
                    params![group.name, group.icon],
                )?;
            }
        }

        // 导入订阅源
        let mut feed_id_map = std::collections::HashMap::new();
        for feed in import_data.feeds {
            // 检查订阅源是否已存在
            let feed_exists = tx
                .query_row(
                    "SELECT id FROM feeds WHERE url = ?",
                    params![feed.url],
                    |row| row.get::<_, i64>(0)
                )
                .optional()?;
            
            let new_id = if let Some(existing_id) = feed_exists {
                // 订阅源已存在，更新记录
                            tx.execute(
                                "UPDATE feeds SET title = ?, group_id = ?, last_updated = ?, description = ?, language = ?, link = ?, favicon = ?, auto_update = ?, enable_notification = ?, ai_auto_translate = ? WHERE url = ?",
                                params![
                                    feed.title,
                                    feed.group_id,
                                    feed.last_updated,
                                    feed.description,
                                    feed.language,
                                    feed.link,
                                    feed.favicon,
                                    feed.auto_update as i8,
                                    feed.enable_notification as i8,
                                    feed.ai_auto_translate as i8,
                                    feed.url
                                ],
                            )?;
                            existing_id
                        } else {
                            // 订阅源不存在，插入新记录，使用RETURNING子句获取ID
                            tx.query_row(
                                "INSERT INTO feeds (title, url, group_id, last_updated, description, language, link, favicon, auto_update, enable_notification, ai_auto_translate) 
                                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) 
                                 RETURNING id",
                                params![
                                    feed.title,
                                    feed.url,
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
                                |row| row.get::<_, i64>(0)
                            )?
                        };

            // 获取新的ID并建立映射
            feed_id_map.insert(feed.id, new_id);
        }

        // 导入文章
        for article in import_data.articles {
            // 使用新的feed_id
            let new_feed_id = feed_id_map
                .get(&article.feed_id)
                .copied()
                .unwrap_or(article.feed_id);

            // 先检查文章是否已存在
            let article_exists = tx
                .query_row(
                    "SELECT id FROM articles WHERE guid = ?",
                    params![article.guid],
                    |row| row.get::<_, i64>(0)
                )
                .optional()?;
            
            if article_exists.is_none() {
                // 文章不存在，插入新文章
                // 直接使用chrono::DateTime类型，DuckDB驱动支持chrono特性
                tx.execute(
                    "INSERT INTO articles 
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
        }

        tx.commit()?;
        Ok(())
    }
    
    /// 导出数据为SQL文件，支持DuckDB
    #[allow(unused)]
    pub async fn export_to_sql(&self, export_path: &PathBuf) -> anyhow::Result<()>
    {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        let mut sql_content = String::new();
        
        // 添加文件头注释
        sql_content.push_str("-- RSS Rust Reader 数据库导出");
        sql_content.push('\n');
        sql_content.push_str("-- 导出时间: ");
        sql_content.push_str(&chrono::Utc::now().to_rfc3339());
        sql_content.push('\n');
        sql_content.push_str("-- 支持DuckDB");
        sql_content.push('\n');
        sql_content.push('\n');
        
        // 按依赖关系排序的表名列表
        let tables = ["feed_groups", "feeds", "articles", "settings"];
        
        // 1. 生成表结构创建语句
        sql_content.push_str("-- 表结构创建语句");
        sql_content.push('\n');
        for table in tables.iter() {
            sql_content.push_str(&self.generate_create_table_sql(&conn, table)?);
            sql_content.push('\n');
        }
        
        // 2. 生成数据插入语句
        sql_content.push('\n');
        sql_content.push_str("-- 数据插入语句");
        sql_content.push('\n');
        for table in tables.iter() {
            sql_content.push_str(&self.generate_insert_sql(&conn, table)?);
            sql_content.push('\n');
        }
        
        // 3. 生成索引创建语句
        sql_content.push('\n');
        sql_content.push_str("-- 索引创建语句");
        sql_content.push('\n');
        for table in tables.iter() {
            sql_content.push_str(&self.generate_create_index_sql(&conn, table)?);
            sql_content.push('\n');
        }
        
        // 写入SQL文件
        std::fs::write(export_path, sql_content)?;
        
        Ok(())
    }
    
    /// 生成CREATE TABLE语句
    #[allow(unused)]
    fn generate_create_table_sql(&self, conn: &Connection, table_name: &str) -> anyhow::Result<String>
    {
        let mut result = String::new();
        
        // 获取表结构信息
        let schema_sql = "SELECT ordinal_position - 1 as cid, 
                                      column_name as name, 
                                      data_type as type, 
                                      CASE WHEN is_nullable = 'NO' THEN 1 ELSE 0 END as notnull,
                                      column_default as dflt_value,
                                      CASE WHEN constraint_name = 'PRIMARY' THEN 1 ELSE 0 END as pk
                               FROM information_schema.columns
                               WHERE table_name = ?
                               ORDER BY ordinal_position".to_string();
        let mut stmt = conn.prepare(&schema_sql)?;
        let columns = stmt.query_map([table_name], |row| {
            Ok((
                row.get::<_, i32>(0)?, // cid
                row.get::<_, String>(1)?, // name
                row.get::<_, String>(2)?, // type
                row.get::<_, i32>(3)?, // notnull
                row.get::<_, Option<String>>(4)?, // dflt_value
                row.get::<_, i32>(5)?, // pk
            ))
        })?
        .collect::<DuckDBResult<Vec<_>>>()?;
        
        // 生成CREATE TABLE语句
        result.push_str(&format!("CREATE TABLE IF NOT EXISTS {table_name} (\n"));
        
        let mut column_defs = Vec::new();
        for (_cid, name, type_, notnull, dflt_value, pk) in columns {
            let mut col_def = format!("    {name} {type_}");
            
            // 处理主键
            if pk == 1 {
                col_def.push_str(" PRIMARY KEY");
                // DuckDB支持AUTOINCREMENT，但语法略有不同
                if type_ == "INTEGER" {
                    col_def.push_str(" AUTOINCREMENT");
                }
            }
            
            // 处理NOT NULL约束
            if notnull == 1 && pk == 0 {
                col_def.push_str(" NOT NULL");
            }
            
            // 处理默认值
            if let Some(default) = dflt_value {
                col_def.push_str(&format!(" DEFAULT {default}"));
            }
            
            column_defs.push(col_def);
        }
        
        // 处理外键约束
        if table_name == "feeds" {
            column_defs.push("    FOREIGN KEY (group_id) REFERENCES feed_groups(id) ON DELETE SET NULL".to_string());
        } else if table_name == "articles" {
            column_defs.push("    FOREIGN KEY (feed_id) REFERENCES feeds(id) ON DELETE CASCADE".to_string());
        }
        
        result.push_str(&column_defs.join(",\n"));
        result.push_str("\n);");
        
        Ok(result)
    }
    
    /// 生成INSERT INTO语句
    #[allow(unused)]
    fn generate_insert_sql(&self, conn: &Connection, table_name: &str) -> anyhow::Result<String>
    {
        let mut result = String::new();
        
        // 获取表的所有列名
        let schema_sql = "SELECT column_name FROM information_schema.columns WHERE table_name = ? ORDER BY ordinal_position".to_string();
        let mut stmt = conn.prepare(&schema_sql)?;
        let columns = stmt.query_map([table_name], |row| {
            row.get::<_, String>(0) // column_name
        })?
        .collect::<DuckDBResult<Vec<_>>>()?;
        
        // 构建SELECT语句
        let select_sql = format!("SELECT {} FROM {}", columns.join(", "), table_name);
        let mut stmt = conn.prepare(&select_sql)?;
        
        // 生成INSERT语句
        let columns_str = columns.join(", ");
        result.push_str(&format!("-- 插入 {} 表数据\n", table_name));
        
        // 遍历所有行
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut values_str = Vec::new();
            
            // 遍历所有列
            for i in 0..columns.len() {
                // 使用ValueRef动态获取值类型
                let value_ref = row.get_ref(i)?;
                
                let value_str = match value_ref {
                    ValueRef::Null => "NULL".to_string(),
                    ValueRef::BigInt(i) => i.to_string(),
                    ValueRef::Double(r) => r.to_string(),
                    ValueRef::Text(t) => {
                        let s = String::from_utf8_lossy(t).to_string();
                        let escaped = self.escape_sql_string(&s);
                        format!("'{}'", escaped)
                    },
                    ValueRef::Blob(b) => {
                        let s = String::from_utf8_lossy(b).to_string();
                        let escaped = self.escape_sql_string(&s);
                        format!("'{}'", escaped)
                    },
                    _ => "NULL".to_string(), // 处理其他类型
                };
                
                values_str.push(value_str);
            }
            
            result.push_str(&format!("INSERT INTO {table_name} ({columns_str}) VALUES ({})\n", values_str.join(", ")));
        }
        
        // 如果没有生成任何INSERT语句，说明没有数据
        if result == format!("-- 插入 {} 表数据\n", table_name) {
            return Ok(String::new());
        }
        
        Ok(result)
    }
    
    /// 生成CREATE INDEX语句
    #[allow(unused)]
    fn generate_create_index_sql(&self, conn: &Connection, table_name: &str) -> anyhow::Result<String>
    {
        let mut result = String::new();
        
        // 使用DuckDB兼容的方式获取索引信息，不使用SQLite特定的PRAGMA
        let indexes_sql = "
            SELECT 
                i.index_name,
                i.is_unique
            FROM 
                information_schema.columns c
            JOIN 
                information_schema.indexes i ON c.table_name = i.table_name AND c.table_schema = i.table_schema
            WHERE 
                c.table_name = ?
            GROUP BY 
                i.index_name, i.is_unique
        ";
        
        let mut stmt = conn.prepare(indexes_sql)?;
        let indexes = stmt.query_map(params![table_name], |row| {
            Ok((
                row.get::<_, String>(0)?, // index_name
                row.get::<_, bool>(1)?, // is_unique
            ))
        })?
        .collect::<DuckDBResult<Vec<_>>>()?;
        
        for (index_name, is_unique) in indexes {
            // 使用DuckDB兼容的方式获取索引列信息
            let columns_sql = "
                SELECT 
                    c.column_name
                FROM 
                    information_schema.columns c
                JOIN 
                    information_schema.indexes i ON c.table_name = i.table_name AND c.table_schema = i.table_schema
                WHERE 
                    i.index_name = ?
                ORDER BY 
                    i.ordinal_position
            ";
            
            let mut columns_stmt = conn.prepare(columns_sql)?;
            let index_columns = columns_stmt.query_map(params![&index_name], |row| {
                row.get::<_, String>(0) // column_name
            })?
            .collect::<DuckDBResult<Vec<_>>>()?;
            
            let columns_str = index_columns.join(", ");
            
            // 生成CREATE INDEX语句
            let unique_str = if is_unique { "UNIQUE " } else { "" };
            result.push_str(&format!("CREATE {}INDEX IF NOT EXISTS {} ON {} ({})\n", 
                unique_str, index_name, table_name, columns_str));
        }
        
        Ok(result)
    }
    
    /// 转义SQL字符串
    #[allow(unused)]
    fn escape_sql_string(&self, s: &str) -> String
    {
        s.replace("'", "''")
         .replace("\"", "\\\"")
         .replace("\n", "\\n")
         .replace("\r", "\\r")
         .replace("\t", "\\t")
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
        retention_days: u32, // 添加保留天数参数
    ) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut new_articles_count = 0;

        // 计算清理日期
        let cutoff_date = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);

        for article in articles {
            // 检查文章发布时间是否超过保留天数（仅对非收藏文章生效）
            if !article.is_starred && article.pub_date < cutoff_date {
                continue; // 跳过超过保留天数的非收藏文章
            }

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
                
                // 添加新文章，使用INSERT，因为已经确保了不会插入重复的文章
                // 直接使用chrono::DateTime类型，DuckDB驱动支持chrono特性
                let result = tx.execute(
                    "INSERT INTO articles (feed_id, title, link, author, pub_date, content, 
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
                        article.is_read,
                        article.is_starred,
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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles WHERE feed_id = ? ORDER BY pub_date DESC"
        )?;

        let articles = stmt
            .query_map(params![feed_id], |row| {
                // 获取pub_date字段，从字符串解析为DateTime<Utc>，增强容错性
                let pub_date = match row.get::<_, Option<String>>(5)? {
                    Some(pub_date_str) => {
                        // 尝试多种时间格式解析
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&pub_date_str) {
                            dt.with_timezone(&chrono::Utc)
                        } else if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&pub_date_str) {
                            dt.with_timezone(&chrono::Utc)
                        } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&pub_date_str, "%Y-%m-%d %H:%M:%S.%f") {
                            // 支持ISO格式：2025-12-16 03:45:35.063049
                            chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                        } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&pub_date_str, "%Y-%m-%d %H:%M:%S") {
                            // 支持ISO格式：2025-12-16 03:45:35
                            chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc)
                        } else {
                            // 所有解析尝试都失败，使用当前时间作为默认值
                            log::warn!("无法解析pub_date: {}, 使用当前时间作为默认值", pub_date_str);
                            chrono::Utc::now()
                        }
                    },
                    None => {
                        // 数据库中为NULL，使用当前时间作为默认值
                        log::warn!("pub_date为NULL，使用当前时间作为默认值");
                        chrono::Utc::now()
                    }
                };
                
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date,
                    content: row.get(6)?,
                    summary: row.get(7)?,
                    is_read: row.get(8)?,
                    is_starred: row.get(9)?,
                    source: row.get(10)?,
                    guid: row.get(11)?,
                })
            })?
            .collect::<DuckDBResult<Vec<Article>>>()?;

        Ok(articles)
    }

    /// 更新文章阅读状态
    #[allow(unused)]
    pub async fn update_article_read_status(
        &mut self,
        article_id: i64,
        read: bool,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        conn.execute(
            "UPDATE articles SET is_starred = ? WHERE id = ?",
            params![starred as i32, article_id],
        )?;

        Ok(())
    }

    /// 更新文章内容
    pub async fn update_article_content(
        &mut self,
        article_id: i64,
        new_content: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        conn.execute(
            "UPDATE articles SET content = ? WHERE id = ?",
            params![new_content, article_id],
        )?;

        Ok(())
    }

    /// 获取所有未读文章数量
    #[allow(unused)]
    pub async fn get_unread_count(&self) -> anyhow::Result<u32> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE feed_id = ? AND is_read = 0",
            params![feed_id],
            |row| row.get(0),
        )?;

        Ok(count)
    }
   
    /// 获取单个文章
    #[allow(dead_code)]
    pub async fn get_article(&self, article_id: i64) -> anyhow::Result<Article> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        let article = conn.query_row(
            "SELECT id, feed_id, title, link, author, pub_date, content, summary, is_read, is_starred, source, guid 
             FROM articles WHERE id = ?",
            params![article_id],
            |row| {
                // 获取pub_date字段，从字符串解析为DateTime<Utc>
                let pub_date_str: String = row.get(5)?;
                let pub_date = match chrono::DateTime::parse_from_rfc3339(&pub_date_str) {
                    Ok(dt) => dt.with_timezone(&chrono::Utc),
                    Err(e) => return Err(duckdb::Error::ToSqlConversionFailure(Box::new(e))),
                };
                
                Ok(Article {
                    id: row.get(0)?,
                    feed_id: row.get(1)?,
                    title: row.get(2)?,
                    link: row.get(3)?,
                    author: row.get(4)?,
                    pub_date,
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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        conn.execute("DELETE FROM articles WHERE feed_id = ? AND is_starred = 0", params![feed_id])?;

        Ok(())
    }

    // delete_article方法已在文件上方定义

    /// 批量标记文章为已读
    pub async fn mark_all_as_read(&mut self, feed_id: Option<i64>) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
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
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        conn.execute("DELETE FROM articles WHERE id = ? AND is_starred = 0", params![article_id])?;

        Ok(())
    }

    /// 批量删除文章
    pub async fn delete_all_articles(&mut self, feed_id: Option<i64>) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));

        match feed_id {
            Some(id) => {
                conn.execute("DELETE FROM articles WHERE feed_id = ? AND is_starred = 0", params![id])?;
            }
            None => {
                conn.execute("DELETE FROM articles WHERE is_starred = 0", [])?;
            }
        }

        Ok(())
    }

    /// 批量删除指定ID的文章
    pub async fn delete_articles(&mut self, article_ids: &[i64]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|_| panic!("无法获取数据库连接锁"));
        for &id in article_ids {
            conn.execute("DELETE FROM articles WHERE id = ? AND is_starred = 0", params![id])?;
        }
        Ok(())
    }
}
