// 数据模型模块

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// RSS源数据模型
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Feed {
    /// 唯一标识符
    pub id: i64,

    /// 标题
    pub title: String,

    /// RSS URL
    pub url: String,

    /// 分组名称
    pub group: String,

    /// 最后更新时间
    pub last_updated: Option<DateTime<Utc>>,

    /// 描述
    pub description: String,

    /// 语言
    pub language: String,

    /// 网站链接
    pub link: String,

    /// 网站图标
    pub favicon: Option<String>,

    /// 是否自动更新
    pub auto_update: bool,

    /// 是否启用通知
    pub enable_notification: bool,

    /// 是否启用AI自动翻译
    pub ai_auto_translate: bool,
}

/// 文章数据模型
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Article {
    /// 唯一标识符
    pub id: i64,

    /// 所属RSS源ID
    pub feed_id: i64,

    /// 标题
    pub title: String,

    /// 链接
    pub link: String,

    /// 作者
    pub author: String,

    /// 发布日期
    pub pub_date: DateTime<Utc>,

    /// 内容
    pub content: String,

    /// 摘要
    pub summary: String,

    /// 是否已读
    pub is_read: bool,

    /// 是否已收藏
    pub is_starred: bool,

    /// 文章来源
    pub source: String,

    /// 全局唯一标识符
    pub guid: String,
}

/// RSS源分组
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FeedGroup {
    /// 唯一标识符
    pub id: i64,

    /// 分组名称
    pub name: String,

    /// 分组图标
    pub icon: Option<String>,
}

/// 应用程序状态
#[derive(Debug, Clone, Default)]
#[allow(unused)]
pub struct AppState {
    /// 已加载的RSS源
    pub feeds: Vec<Feed>,

    /// 当前选中的RSS源ID
    pub selected_feed_id: Option<i64>,

    /// 当前显示的文章
    pub articles: Vec<Article>,

    /// 当前选中的文章ID
    pub selected_article_id: Option<i64>,

    /// 选中的文章ID集合
    pub selected_articles: std::collections::HashSet<i64>,

    /// 是否正在刷新
    pub is_refreshing: bool,

    /// 刷新进度（0-100）
    pub refresh_progress: u8,
}

#[allow(unused)]
impl AppState {
    /// 选择文章
    pub fn select_article(&mut self, article_id: i64) {
        self.selected_articles.insert(article_id);
    }

    /// 取消选择文章
    pub fn deselect_article(&mut self, article_id: i64) {
        self.selected_articles.remove(&article_id);
    }

    /// 切换文章选择状态
    pub fn toggle_select_article(&mut self, article_id: i64) {
        if self.selected_articles.contains(&article_id) {
            self.deselect_article(article_id);
        } else {
            self.select_article(article_id);
        }
    }

    /// 全选文章
    pub fn select_all_articles(&mut self) {
        self.selected_articles.clear();
        for article in &self.articles {
            self.selected_articles.insert(article.id);
        }
    }

    /// 取消全选文章
    pub fn deselect_all_articles(&mut self) {
        self.selected_articles.clear();
    }

    /// 获取选中的文章
    pub fn get_selected_articles(&self) -> Vec<&Article> {
        self.articles
            .iter()
            .filter(|article| self.selected_articles.contains(&article.id))
            .collect()
    }

    /// 获取选中的文章数量
    pub fn selected_articles_count(&self) -> usize {
        self.selected_articles.len()
    }

    /// 检查文章是否被选中
    pub fn is_article_selected(&self, article_id: i64) -> bool {
        self.selected_articles.contains(&article_id)
    }
}

/// 文章过滤选项
#[derive(Debug, Clone)]
#[allow(unused)]
pub struct ArticleFilter {
    /// 仅显示未读
    pub unread_only: bool,

    /// 仅显示收藏
    pub starred_only: bool,

    /// 搜索关键词
    pub search_term: String,

    /// 排序方式
    pub sort_by: ArticleSort,

    /// 排序方向
    pub sort_descending: bool,
}

/// 文章排序方式
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(unused)]
pub enum ArticleSort {
    /// 按发布日期
    PublishedDate,
    /// 按标题
    Title,
    /// 按作者
    Author,
}

impl Default for ArticleFilter {
    fn default() -> Self {
        Self {
            unread_only: false,
            starred_only: false,
            search_term: String::new(),
            sort_by: ArticleSort::PublishedDate,
            sort_descending: true,
        }
    }
}
