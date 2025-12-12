// 搜索模块 - 提供内存中的文章全文搜索功能，优化支持中文搜索

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::models::Article;

/// 单个订阅源的文章索引
struct PerFeedIndex {
    // 倒排索引: 关键字 -> 文章ID集合
    inverted_index: HashMap<String, HashSet<i64>>,

    // 存储所有文章，以便快速获取文章内容
    articles: HashMap<i64, Article>,

    // 存储文章总数
    total_articles: u64,

    // 存储文章的完整文本用于模糊匹配
    article_full_texts: HashMap<i64, String>,
}

impl PerFeedIndex {
    /// 创建一个新的单个订阅源文章索引
    fn new() -> Self {
        PerFeedIndex {
            inverted_index: HashMap::new(),
            articles: HashMap::new(),
            total_articles: 0,
            article_full_texts: HashMap::new(),
        }
    }

    /// 计算包含指定词的文档数量
    fn count_docs_with_term(&self, term: &str) -> usize {
        self.inverted_index
            .get(term)
            .map_or(1, |ids| ids.len())
            .max(1)
    }

    /// 查找包含任意查询词的文章ID（OR匹配）
    fn find_articles_with_any_terms(&self, terms: &[String]) -> HashSet<i64> {
        if terms.is_empty() {
            return HashSet::new();
        }

        let mut matching_ids = HashSet::new();

        for term in terms {
            if let Some(ids) = self.inverted_index.get(term) {
                matching_ids.extend(ids);
            }
        }

        matching_ids
    }

    /// 查找包含所有查询词的文章ID（AND条件）
    fn find_articles_with_all_terms(&self, terms: &[String]) -> HashSet<i64> {
        if terms.is_empty() {
            return HashSet::new();
        }

        // 获取第一个词的匹配文章ID集合
        let mut matching_ids = if let Some(ids) = self.inverted_index.get(&terms[0]) {
            ids.clone()
        } else {
            return HashSet::new();
        };

        // 对于剩余的词，只保留同时包含这些词的文章ID
        for term in &terms[1..] {
            if let Some(ids) = self.inverted_index.get(term) {
                matching_ids.retain(|id| ids.contains(id));
                if matching_ids.is_empty() {
                    return matching_ids;
                }
            } else {
                return HashSet::new();
            }
        }

        matching_ids
    }

    /// 添加或更新文章到索引
    fn add_article(&mut self, article: Article) {
        let article_id = article.id;

        // 构建完整文本用于模糊匹配
        let full_text = format!(
            "{} {} {} {}",
            article.title, article.content, article.summary, article.author
        )
        .to_lowercase();
        self.article_full_texts.insert(article_id, full_text);

        // 更新索引（使用引用，避免克隆）
        self.index_article(&article);

        // 保存文章（最后克隆，避免重复操作）
        self.articles.insert(article_id, article);
        self.total_articles += 1;
    }

    /// 从索引中删除文章
    fn remove_article(&mut self, article_id: i64) {
        // 如果文章不存在，则直接返回
        if !self.articles.contains_key(&article_id) {
            return;
        }

        // 删除文章
        self.articles.remove(&article_id);
        self.article_full_texts.remove(&article_id);

        // 从倒排索引中删除该文章的所有引用
        for article_ids in self.inverted_index.values_mut() {
            article_ids.remove(&article_id);
        }

        // 清理空的条目以节省内存
        self.inverted_index
            .retain(|_term, article_ids| !article_ids.is_empty());
    }

    /// 清空索引
    #[allow(unused)]
    fn clear(&mut self) {
        self.inverted_index.clear();
        self.articles.clear();
        self.article_full_texts.clear();
        self.total_articles = 0;
    }

    /// 计算文章的相关度分数
    fn calculate_relevance_score(
        &self,
        article: &Article,
        terms: &[String],
        query: &str,
        exact_matches: &HashSet<i64>,
    ) -> f64 {
        let mut score = 0.0;
        let article_id = article.id;

        // 获取文章的完整文本
        let article_text = if let Some(text) = self.article_full_texts.get(&article_id) {
            text
        } else {
            return score;
        };

        let title_lower = article.title.to_lowercase();

        // 检查是否在精确匹配集合中
        if exact_matches.contains(&article_id) {
            score += 50.0;
        }

        // 检查是否包含完整的查询短语
        if article_text.contains(query) {
            score += 30.0;
        }

        // 标题中包含完整查询短语给予更高权重
        if title_lower.contains(query) {
            score += 40.0;
        }

        // 计算TF-IDF相关度
        let total_docs = self.articles.len() as f64;
        let article_terms = self.tokenize(article_text);

        // 计算词频
        let mut term_count = HashMap::new();
        for term in &article_terms {
            *term_count.entry(term).or_insert(0) += 1;
        }

        // 计算每个查询词的TF-IDF分数
        let article_term_count = article_terms.len() as f64;
        for query_term in terms {
            if let Some(count) = term_count.get(query_term) {
                let tf = *count as f64 / article_term_count;
                let doc_count = self.count_docs_with_term(query_term) as f64;
                let idf = total_docs.ln() / doc_count.ln().max(0.0001);

                // 基本TF-IDF分数
                score += tf * idf * 2.0;

                // 标题中包含该词的额外加分
                if title_lower.contains(query_term) {
                    score += 1.5 * idf;
                }
            }
        }

        // 考虑词的位置权重 - 标题中的词比内容中的词更重要
        for query_term in terms {
            if title_lower.contains(query_term) {
                score += 3.0;
            }
        }

        score
    }

    /// 模糊搜索 - 在完整文本中直接搜索
    fn fuzzy_search(&self, query: &str) -> Vec<Article> {
        let mut results = Vec::new();

        for (article_id, full_text) in &self.article_full_texts {
            if let Some(article) = self.articles.get(article_id) {
                // 检查是否包含查询字符串（不区分大小写）
                if full_text.contains(query) {
                    results.push(article.clone());
                }
            }
        }

        // 简单按发布日期排序
        results.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));
        results
    }

    /// 内部方法：将文章内容分词并添加到倒排索引
    fn index_article(&mut self, article: &Article) {
        let article_id = article.id;

        // 合并所有需要索引的字段
        let text_to_index = format!(
            "{}{}{}{}",
            article.title, article.content, article.summary, article.author
        )
        .to_lowercase();

        // 分词
        let terms = self.tokenize(&text_to_index);

        // 更新倒排索引
        for term in terms {
            self.inverted_index
                .entry(term)
                .or_default()
                .insert(article_id);
        }
    }

    /// 内部方法：文本分词（改进版，更好地支持中文任意位置搜索）
    fn tokenize(&self, text: &str) -> Vec<String> {
        let text = text.to_lowercase();
        let mut terms = Vec::new();

        // 改进的分词策略：生成所有可能的2-4字符长度的子串
        // 这样可以在任意位置匹配到关键词

        // 首先，添加完整的单词（英文）
        for word in text.split_whitespace() {
            if word.len() >= 2 {
                terms.push(word.to_string());
            }
        }

        // 对于中文字符串，生成所有可能的2-4字组合
        let chars: Vec<char> = text.chars().collect();

        // 生成2字符组合
        for i in 0..chars.len().saturating_sub(1) {
            let term: String = chars[i..i + 2].iter().collect();
            if self.is_meaningful_term(&term) {
                terms.push(term);
            }
        }

        // 生成3字符组合
        for i in 0..chars.len().saturating_sub(2) {
            let term: String = chars[i..i + 3].iter().collect();
            if self.is_meaningful_term(&term) {
                terms.push(term);
            }
        }

        // 生成4字符组合（对于较长的中文短语）
        for i in 0..chars.len().saturating_sub(3) {
            let term: String = chars[i..i + 4].iter().collect();
            if self.is_meaningful_term(&term) {
                terms.push(term);
            }
        }

        // 移除重复的单词
        let mut unique_terms = HashSet::new();
        terms.retain(|term| unique_terms.insert(term.clone()));

        terms
    }

    /// 检查术语是否有意义（过滤掉无意义的字符组合）
    fn is_meaningful_term(&self, term: &str) -> bool {
        if term.len() < 2 {
            return false;
        }

        // 检查是否包含足够的中文字符或字母数字
        let mut meaningful_chars = 0;
        for c in term.chars() {
            if self.is_chinese_char(c) || c.is_alphanumeric() {
                meaningful_chars += 1;
            }
        }

        meaningful_chars >= term.len() / 2
    }

    /// 检查字符是否为中文字符
    fn is_chinese_char(&self, c: char) -> bool {
        match c {
            // 基本汉字范围（CJK Unified Ideographs）
            '\u{4e00}'..='\u{9fff}' => true,
            // 扩展A区汉字
            '\u{3400}'..='\u{4dbf}' => true,
            // 常见中文标点符号
            '\u{3000}'..='\u{303f}' | // CJK符号和标点
            '\u{ff00}'..='\u{ffef}'   // 半角及全角形式
            => true,
            // 其他中文字符范围可以根据需要添加
            _ => false,
        }
    }

    /// 搜索包含指定查询词的文章，使用混合策略：先精确匹配，再模糊匹配
    fn search(&self, query: &str) -> Vec<Article> {
        if query.trim().is_empty() {
            return Vec::new();
        }

        let query = query.trim();
        let query_lower = query.to_lowercase();

        // 将查询分割为关键词
        let terms = self.tokenize(&query_lower);

        if terms.is_empty() {
            return Vec::new();
        }

        // 策略1: 精确匹配（AND条件）
        let exact_matches = self.find_articles_with_all_terms(&terms);

        // 策略2: 部分匹配（OR条件）
        let partial_matches = if exact_matches.is_empty() {
            self.find_articles_with_any_terms(&terms)
        } else {
            exact_matches.clone()
        };

        if partial_matches.is_empty() {
            // 策略3: 模糊匹配 - 在完整文本中搜索
            return self.fuzzy_search(&query_lower);
        }

        // 计算相关度分数
        let mut scored_articles: Vec<(Article, f64)> = partial_matches
            .iter()
            .filter_map(|id| {
                if let Some(article) = self.articles.get(id) {
                    let score = self.calculate_relevance_score(
                        article,
                        &terms,
                        &query_lower,
                        &exact_matches,
                    );
                    Some((article.clone(), score))
                } else {
                    None
                }
            })
            .collect();

        // 按相关度分数和发布日期排序
        scored_articles.sort_by(|a, b| {
            let score_cmp = b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal);
            if score_cmp == std::cmp::Ordering::Equal {
                b.0.pub_date.cmp(&a.0.pub_date)
            } else {
                score_cmp
            }
        });

        // 只返回文章，不返回分数
        scored_articles
            .into_iter()
            .map(|(article, _score)| article)
            .collect()
    }
}

/// 文章索引结构体 - 实现按订阅源分类的倒排索引用于快速全文搜索
pub struct ArticleIndex {
    // 按订阅源ID分类的索引: 订阅源ID -> 该订阅源的文章索引
    feed_indices: HashMap<i64, PerFeedIndex>,
}

impl ArticleIndex {
    /// 创建一个新的文章索引
    pub fn new() -> Self {
        ArticleIndex {
            feed_indices: HashMap::new(),
        }
    }

    /// 实现Default trait以支持创建默认实例
    #[allow(unused)]
    pub fn default() -> Self {
        Self::new()
    }

    /// 添加或更新文章到索引
    pub fn add_article(&mut self, article: Article) {
        let feed_id = article.feed_id;

        // 获取或创建该订阅源的索引
        let feed_index = self
            .feed_indices
            .entry(feed_id)
            .or_insert_with(PerFeedIndex::new);

        // 添加文章到该订阅源的索引
        feed_index.add_article(article);
    }

    /// 批量添加文章到索引
    pub fn add_articles(&mut self, articles: Vec<Article>) {
        for article in articles {
            self.add_article(article);
        }
    }

    /// 从索引中删除文章
    pub fn remove_article(&mut self, article_id: i64, feed_id: i64) {
        // 如果该订阅源的索引不存在，则直接返回
        if let Some(feed_index) = self.feed_indices.get_mut(&feed_id) {
            feed_index.remove_article(article_id);
        }
    }

    /// 删除特定订阅源的所有文章
    pub fn remove_articles_by_feed(&mut self, feed_id: i64) {
        // 删除该订阅源的整个索引
        self.feed_indices.remove(&feed_id);
    }

    /// 清空索引
    #[allow(unused)]
    pub fn clear(&mut self) {
        self.feed_indices.clear();
    }

    /// 搜索包含指定查询词的文章，使用混合策略：先精确匹配，再模糊匹配
    /// feed_id: -1 表示搜索所有订阅源，否则搜索指定订阅源
    pub fn search(&self, query: &str, feed_id: i64) -> Vec<Article> {
        if query.trim().is_empty() {
            return Vec::new();
        }

        let mut all_results = Vec::new();

        // 根据feed_id决定搜索范围
        if feed_id == -1 {
            // 搜索所有订阅源
            for feed_index in self.feed_indices.values() {
                let results = feed_index.search(query);
                all_results.extend(results);
            }
        } else {
            // 只搜索指定订阅源
            if let Some(feed_index) = self.feed_indices.get(&feed_id) {
                all_results = feed_index.search(query);
            }
        }

        // 按发布日期排序（相关度已经在每个订阅源内部计算过了）
        all_results.sort_by(|a, b| b.pub_date.cmp(&a.pub_date));

        all_results
    }

    /// 获取索引中的文章总数
    #[allow(unused)]
    pub fn len(&self) -> usize {
        self.feed_indices
            .values()
            .map(|index| index.articles.len())
            .sum()
    }

    /// 检查索引是否为空
    #[allow(unused)]
    pub fn is_empty(&self) -> bool {
        self.feed_indices.is_empty()
            || self
                .feed_indices
                .values()
                .all(|index| index.articles.is_empty())
    }
}

// 搜索结果缓存条目
struct SearchCacheEntry {
    results: Vec<Article>,
    timestamp: Instant,
}

/// 搜索管理器 - 用于在应用中管理和提供搜索功能，包含搜索缓存
pub struct SearchManager {
    index: Arc<Mutex<ArticleIndex>>,
    // 搜索结果缓存，使用RwLock以支持并发读取
    search_cache: Arc<RwLock<HashMap<String, SearchCacheEntry>>>,
    // 缓存有效期（秒）
    cache_ttl: Duration,
}

impl SearchManager {
    /// 创建新的搜索管理器
    pub fn new() -> Self {
        SearchManager {
            index: Arc::new(Mutex::new(ArticleIndex::new())),
            search_cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_secs(300), // 缓存5分钟
        }
    }

    /// 添加文章到索引
    pub async fn add_article(&self, article: Article) {
        let mut index = self.index.lock().await;
        index.add_article(article);
        // 清除缓存，因为索引已更新
        self.clear_cache().await;
    }

    /// 批量添加文章到索引
    pub async fn add_articles(&self, articles: Vec<Article>) {
        let mut index = self.index.lock().await;
        index.add_articles(articles);
        // 清除缓存，因为索引已更新
        self.clear_cache().await;
    }

    /// 从索引中删除文章
    pub async fn remove_article(&self, article_id: i64, feed_id: i64) {
        let mut index = self.index.lock().await;
        index.remove_article(article_id, feed_id);
        // 清除缓存，因为索引已更新
        self.clear_cache().await;
    }

    /// 删除特定订阅源的所有文章索引
    pub async fn remove_articles_by_feed(&self, feed_id: i64) {
        let mut index = self.index.lock().await;
        index.remove_articles_by_feed(feed_id);
        // 清除缓存，因为索引已更新
        self.clear_cache().await;
    }

    /// 清空索引
    #[allow(unused)]
    pub async fn clear(&self) {
        let mut index = self.index.lock().await;
        index.clear();
        // 清除缓存
        self.clear_cache().await;
    }

    /// 搜索文章，优先使用缓存
    pub async fn search(&self, query: &str, feed_id: i64) -> Vec<Article> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }

        // 尝试从缓存获取结果，缓存键包含查询和feed_id
        let cache_key = format!("{}:{}", query, feed_id);
        if let Some(results) = self.get_cached_results(&cache_key).await {
            return results;
        }

        // 缓存未命中，执行实际搜索
        let index = self.index.lock().await;
        let results = index.search(query, feed_id);

        // 克隆一份结果返回，另一份用于缓存
        self.cache_results(&cache_key, results.clone()).await;

        results
    }

    /// 获取索引中的文章总数
    #[allow(unused)]
    pub async fn len(&self) -> usize {
        let index = self.index.lock().await;
        index.len()
    }

    /// 检查索引是否为空
    #[allow(unused)]
    pub async fn is_empty(&self) -> bool {
        let index = self.index.lock().await;
        index.is_empty()
    }

    /// 从缓存获取搜索结果
    async fn get_cached_results(&self, cache_key: &str) -> Option<Vec<Article>> {
        if let Ok(cache) = self.search_cache.read()
            && let Some(entry) = cache.get(cache_key)
                && Instant::now().duration_since(entry.timestamp) <= self.cache_ttl {
                    return Some(entry.results.clone());
                }
        None
    }

    /// 缓存搜索结果
    async fn cache_results(&self, cache_key: &str, results: Vec<Article>) {
        if let Ok(mut cache) = self.search_cache.write() {
            cache.insert(
                cache_key.to_string(),
                SearchCacheEntry {
                    results,
                    timestamp: Instant::now(),
                },
            );

            self.cleanup_expired_cache(&mut cache);

            if cache.len() > 100
                && let Some(oldest_key) = cache.keys().next().cloned() {
                    cache.remove(&oldest_key);
                }
        }
    }

    /// 清理过期缓存
    fn cleanup_expired_cache(&self, cache: &mut HashMap<String, SearchCacheEntry>) {
        let now = Instant::now();
        cache.retain(|_, entry| now.duration_since(entry.timestamp) <= self.cache_ttl);
    }

    /// 清除搜索缓存
    async fn clear_cache(&self) {
        if let Ok(mut cache) = self.search_cache.write() {
            cache.clear();
        }
    }
}
