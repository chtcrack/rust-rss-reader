// AI 客户端模块

use futures::StreamExt;
use html_escape::decode_html_entities;
use ammonia::{Builder, UrlRelative};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs::{File, remove_file};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;

// 定义类型别名简化复杂类型
type StreamCallback<'a> = Option<&'a mut (dyn FnMut(&str) -> Result<(), AIClientError> + Send)>;

/// AI 消息角色
enum MessageRole {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageRole::System => write!(f, "system"),
            MessageRole::User => write!(f, "user"),
            MessageRole::Assistant => write!(f, "assistant"),
        }
    }
}

/// AI 消息结构体
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AIMessage {
    /// 消息角色
    pub role: String,
    /// 消息内容
    pub content: String,
}

impl AIMessage {
    /// 创建系统消息
    pub fn system(content: &str) -> Self {
        Self {
            role: MessageRole::System.to_string(),
            content: content.to_string(),
        }
    }

    /// 创建用户消息
    pub fn user(content: &str) -> Self {
        Self {
            role: MessageRole::User.to_string(),
            content: content.to_string(),
        }
    }

    /// 创建助手消息
    pub fn assistant(content: &str) -> Self {
        Self {
            role: MessageRole::Assistant.to_string(),
            content: content.to_string(),
        }
    }
}

/// AI 聊天完成请求结构体
#[derive(Serialize, Debug)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<AIMessage>,
    temperature: Option<f32>,
    max_tokens: Option<usize>,
    stream: bool,
}

/// AI 聊天完成响应选项结构体
#[derive(Deserialize, Debug)]
struct Choice {
    message: AIMessage,
}

/// AI 聊天完成响应结构体
#[derive(Deserialize, Debug)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

/// AI 流式聊天完成响应结构体
#[derive(Deserialize, Debug)]
struct StreamChatCompletionResponse {
    choices: Vec<StreamChoice>,
}

/// AI 流式聊天完成响应选项结构体
#[derive(Deserialize, Debug)]
struct StreamChoice {
    delta: StreamDelta,
    #[allow(unused)]
    finish_reason: Option<String>,
}

/// AI 流式聊天完成响应增量结构体
#[derive(Deserialize, Debug)]
struct StreamDelta {
    content: Option<String>,
    #[allow(unused)]
    role: Option<String>,
}

/// AI 客户端错误类型
#[derive(Debug, thiserror::Error)]
pub enum AIClientError {
    #[error("HTTP 请求错误: {0}")]
    HttpRequest(#[from] reqwest::Error),

    #[error("超时错误")]
    Timeout,

    #[error("响应解析错误: {0}")]
    ResponseParse(#[from] serde_json::Error),

    #[error("API 错误: {0}")]
    ApiError(String),

    #[error("配置错误: {0}")]
    ConfigError(String),
}

/// AI 客户端结构体
#[derive(Clone)]
pub struct AIClient {
    client: Client,
    api_url: String,
    api_key: String,
    model_name: String,
    timeout_duration: Duration,
}

impl AIClient {
    /// 创建新的 AI 客户端
    pub fn new(api_url: &str, api_key: &str, model_name: &str) -> Result<Self, AIClientError> {
        if api_url.trim().is_empty() {
            return Err(AIClientError::ConfigError("API URL 不能为空".to_string()));
        }

        if model_name.trim().is_empty() {
            return Err(AIClientError::ConfigError("模型名称不能为空".to_string()));
        }

        Ok(Self {
            client: Client::new(),
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
            model_name: model_name.to_string(),
            timeout_duration: Duration::from_secs(30),
        })
    }

    /// 发送聊天完成请求
    pub async fn chat_completion(
        &self,
        messages: &[AIMessage],
        temperature: Option<f32>,
        max_tokens: Option<usize>,
        stream_callback: StreamCallback<'_>,
    ) -> Result<String, AIClientError> {
        // 检查 API Key
        if self.api_key.trim().is_empty() {
            return Err(AIClientError::ConfigError("API Key 不能为空".to_string()));
        }

        // 构建请求体
        let request = ChatCompletionRequest {
            model: self.model_name.clone(),
            messages: messages.to_vec(),
            temperature,
            max_tokens,
            stream: stream_callback.is_some(),
        };

        // 构建请求
        let request_builder = self
            .client
            .post(&self.api_url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request);

        // 发送请求并设置超时
        let response = timeout(self.timeout_duration, request_builder.send())
            .await
            .map_err(|_| AIClientError::Timeout)?
            .map_err(AIClientError::HttpRequest)?;

        // 检查响应状态
        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP 错误: {}", status));
            return Err(AIClientError::ApiError(error_text));
        }

        // 根据是否需要流式响应，选择不同的处理方式
        if let Some(callback) = stream_callback {
            // 流式响应处理
            self.handle_stream_response(response, callback).await
        } else {
            // 非流式响应处理
            let completion_response: ChatCompletionResponse = response.json().await?;

            // 提取回复内容
            if let Some(choice) = completion_response.choices.first() {
                Ok(choice.message.content.clone())
            } else {
                Err(AIClientError::ApiError("没有返回有效的回复".to_string()))
            }
        }
    }

    /// 处理流式响应
    async fn handle_stream_response(
        &self,
        response: reqwest::Response,
        callback: &mut (dyn FnMut(&str) -> Result<(), AIClientError> + Send),
    ) -> Result<String, AIClientError> {
        // 获取响应流
        let mut stream = response.bytes_stream();

        // 用于存储完整的响应内容
        let mut full_content = String::new();

        // 用于存储当前正在解析的行
        let mut current_line = String::new();

        // 遍历响应流中的每个字节块
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(AIClientError::HttpRequest)?;
            let chunk_str = String::from_utf8_lossy(&chunk);

            // 将当前块追加到当前行
            current_line.push_str(&chunk_str);

            // 处理所有完整的行
            while let Some(newline_pos) = current_line.find("\n") {
                // 提取完整的行
                let line = current_line.drain(..=newline_pos).collect::<String>();

                // 处理 SSE 格式的行
                if let Some(stripped) = line.strip_prefix("data: ") {
                    // 提取数据部分
                    let data = stripped.trim();

                    // 检查是否是结束标记
                    if data == "[DONE]" {
                        break;
                    }

                    // 解析 JSON 数据
                    match serde_json::from_str::<StreamChatCompletionResponse>(data) {
                        Ok(stream_response) => {
                            // 处理每个选择
                            for choice in stream_response.choices {
                                // 提取内容增量
                                if let Some(content) = choice.delta.content {
                                    // 更新完整内容
                                    full_content.push_str(&content);

                                    // 调用回调函数
                                    callback(&content)?;
                                }
                            }
                        }
                        Err(e) => {
                            log::debug!("解析流式响应失败: {}, 响应内容: {}", e, data);
                            // 忽略解析错误，继续处理下一行
                        }
                    }
                }
            }
        }

        Ok(full_content)
    }

    /// 简单的提问方法，自动添加系统消息
    #[allow(unused)]
    pub async fn ask(
        &self,
        question: &str,
        stream_callback: StreamCallback<'_>,
    ) -> Result<String, AIClientError> {
        let messages = vec![
            AIMessage::system("你是一个有用的助手。请根据提供的内容，给出详细、准确的回答。"),
            AIMessage::user(question),
        ];

        self.chat_completion(&messages, Some(0.7), None, stream_callback)
            .await
    }

    /// 检测文本语言
    pub async fn detect_language(&self, text: &str) -> Result<String, AIClientError> {
        let messages = vec![
            AIMessage::system("你是一个语言检测专家。请仅返回文本的语言代码，如中文返回'zh'，英文返回'en'，日语返回'ja'，韩语返回'ko'，法语返回'fr'，德语返回'de'，西班牙语返回'es'，俄语返回'ru'，其他语言返回对应的ISO 639-1语言代码。不要返回任何解释或其他内容。"),
            AIMessage::user(text),
        ];

        let result = self.chat_completion(&messages, Some(0.0), Some(10), None).await?;
        Ok(result.trim().to_lowercase())
    }

    /// 翻译文本为中文 暂时未使用,可以为以后的功能进行调用
    #[allow(unused)]
    pub async fn translate_to_chinese(&self, text: &str) -> Result<String, AIClientError> {
        let messages = vec![
            AIMessage::system("你是一个专业的翻译官。请将以下文本翻译成中文，保持原意准确，语言流畅自然。不要添加任何解释或其他内容，仅返回翻译结果。"),
            AIMessage::user(text),
        ];

        self.chat_completion(&messages, Some(0.3), None, None).await
    }

    /// 清理HTML内容，只保留p和br标签，移除所有其他HTML标签
    fn sanitize_content(&self, html: &str) -> String {
        // 创建ammonia构建器
        let mut builder = Builder::new();
        
        // 只允许p和br标签
        let allowed_tags = std::collections::HashSet::from(["p", "br"]);
        builder.tags(allowed_tags);
        
        // 不允许任何属性
        builder.tag_attributes(std::collections::HashMap::new());
        
        // 不允许任何协议处理
        builder.url_relative(UrlRelative::PassThrough);
        
        // 不允许任何链接处理
        builder.link_rel(None);
        
        // 清理HTML内容
        let sanitized_html = builder.clean(html).to_string();
        
        // 解码HTML实体
        let decoded = decode_html_entities(&sanitized_html);
        
        // 移除多余的空白字符，保留合理的换行和空格
        
        
        decoded
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 翻译文章标题和内容为中文
    pub async fn translate_article(
        &self,
        title: &str,
        content: &str,
    ) -> Result<(String, String), AIClientError> {
        // 清理标题和内容，确保发送给AI的是纯文本
        let cleaned_title = self.sanitize_content(title);
        let cleaned_content = self.sanitize_content(content);

        let title_prompt = format!("请将以下文章标题翻译成中文，保持原意准确，语言流畅自然：\n\n{}", cleaned_title);
        let content_prompt = format!("请将以下文章内容翻译成中文，保持原意准确，语言流畅自然：\n\n{}", cleaned_content);

        let title_messages = vec![
            AIMessage::system("你是一个专业的翻译官。请将文本翻译成中文，保持原意准确，语言流畅自然。不要添加任何解释或其他内容，仅返回翻译结果。"),
            AIMessage::user(&title_prompt),
        ];

        let content_messages = vec![
            AIMessage::system("你是一个专业的翻译官。请将文本翻译成中文，保持原意准确，语言流畅自然。不要添加任何解释或其他内容，仅返回翻译结果。"),
            AIMessage::user(&content_prompt),
        ];

        let translated_title = self.chat_completion(&title_messages, Some(0.3), None, None).await?;
        let translated_content = self.chat_completion(&content_messages, Some(0.3), None, None).await?;

        Ok((translated_title, translated_content))
    }

    /// 设置超时时间
    #[allow(unused)]
    pub fn set_timeout(&mut self, duration: Duration) {
        self.timeout_duration = duration;
    }

    /// 更新 API 配置
    #[allow(unused)]
    pub fn update_config(
        &mut self,
        api_url: &str,
        api_key: &str,
        model_name: &str,
    ) -> Result<(), AIClientError> {
        if api_url.trim().is_empty() {
            return Err(AIClientError::ConfigError("API URL 不能为空".to_string()));
        }

        if model_name.trim().is_empty() {
            return Err(AIClientError::ConfigError("模型名称不能为空".to_string()));
        }

        self.api_url = api_url.to_string();
        self.api_key = api_key.to_string();
        self.model_name = model_name.to_string();

        Ok(())
    }
}

/// AI 聊天会话管理
#[derive(Clone)]
pub struct AIChatSession {
    client: AIClient,
    messages: Vec<AIMessage>,
    max_history_length: usize,
}

impl AIChatSession {
    /// 创建新的聊天会话
    pub fn new(client: AIClient) -> Self {
        Self {
            client,
            messages: Vec::new(),
            max_history_length: 100, // 默认保留100条消息
        }
    }

    /// 设置系统提示
    pub fn set_system_prompt(&mut self, prompt: &str) {
        // 检查是否已有系统消息
        if let Some(first_message) = self.messages.first()
            && first_message.role == MessageRole::System.to_string() {
                // 更新现有系统消息
                self.messages[0] = AIMessage::system(prompt);
                return;
            }

        // 添加新的系统消息到开头
        self.messages.insert(0, AIMessage::system(prompt));
    }

    /// 发送消息
    pub async fn send_message(
        &mut self,
        content: &str,
        stream_callback: StreamCallback<'_>,
    ) -> Result<String, AIClientError> {
        // 添加用户消息
        self.messages.push(AIMessage::user(content));

        // 确保消息历史不超过最大长度和token限制
        self.trim_history();

        // 估算发送的总token数
        let total_tokens = self.estimate_messages_tokens(&self.messages);
        log::info!("发送消息到AI平台，总token数: {}", total_tokens);

        // 发送请求
        let response = match self
            .client
            .chat_completion(&self.messages, Some(0.7), None, stream_callback)
            .await
        {
            Ok(response) => {
                log::info!("AI平台返回成功响应");
                response
            }
            Err(e) => {
                log::error!("AI平台返回错误: {:?}", e);
                return Err(e);
            }
        };

        // 添加助手回复
        self.messages.push(AIMessage::assistant(&response));

        // 再次确保消息历史不超过最大长度和token限制
        self.trim_history();

        Ok(response)
    }

    /// 获取消息历史
    pub fn get_messages(&self) -> &[AIMessage] {
        &self.messages
    }

    /// 清空消息历史
    pub fn clear_history(&mut self) {
        // 保留系统消息（如果有）
        if let Some(first_message) = self.messages.first() {
            if first_message.role == MessageRole::System.to_string() {
                self.messages = vec![first_message.clone()];
            } else {
                self.messages.clear();
            }
        } else {
            self.messages.clear();
        }
    }

    /// 设置最大历史长度
    #[allow(unused)]
    pub fn set_max_history_length(&mut self, length: usize) {
        self.max_history_length = length;
        self.trim_history();
    }

    /// 添加助手回复
    pub fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(AIMessage::assistant(content));
        self.trim_history();
    }

    /// 获取聊天历史记录文件路径
    fn chat_history_path() -> PathBuf {
        let mut path = if cfg!(target_os = "windows") {
            dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."))
        } else {
            dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
        };

        path.push("rust_rss_reader");

        // 确保目录存在
        std::fs::create_dir_all(&path).ok();

        path.push("ai_chat_history.json");
        path
    }

    /// 保存聊天历史记录到JSON文件
    pub fn save_chat_history(&self) -> Result<(), std::io::Error> {
        let path = Self::chat_history_path();

        let json = serde_json::to_string_pretty(&self.messages)?;
        let mut file = File::create(path)?;
        file.write_all(json.as_bytes())?;

        Ok(())
    }

    /// 从JSON文件加载聊天历史记录
    pub fn load_chat_history(&mut self) -> Result<(), std::io::Error> {
        let path = Self::chat_history_path();

        if let Ok(mut file) = File::open(path) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok()
                && let Ok(messages) = serde_json::from_str(&contents) {
                    self.messages = messages;
                    return Ok(());
                }
        }

        Ok(())
    }

    /// 删除聊天历史记录文件
    pub fn delete_chat_history() -> Result<(), std::io::Error> {
        let path = Self::chat_history_path();

        if path.exists() {
            remove_file(path)?;
        }

        Ok(())
    }

 fn estimate_tokens(&self, text: &str) -> usize {
    let mut estimate = 0;
    let mut ascii_word_len = 0;
    
    for c in text.chars() {
        // 检查是否为中日韩字符
        let is_cjk = self.is_cjk_char(c);
        
        if is_cjk {
            if ascii_word_len > 0 {
                estimate += self.estimate_ascii_word_tokens(ascii_word_len);
                ascii_word_len = 0;
            }
            estimate += 1; // 大多数CJK字符是1个token
        } else if c.is_ascii_alphanumeric() {
            ascii_word_len += 1;
        } else {
            // 处理标点、空格等
            if ascii_word_len > 0 {
                estimate += self.estimate_ascii_word_tokens(ascii_word_len);
                ascii_word_len = 0;
            }
            
            match c {
                ' ' | '\t' => estimate += 1, // 空格通常需要1个token
                '\n' => estimate += 1,       // 换行符
                _ => estimate += 1,          // 其他标点符号
            }
        }
    }
    
    if ascii_word_len > 0 {
        estimate += self.estimate_ascii_word_tokens(ascii_word_len);
    }
    
    // 确保最小值
    estimate.max(1)
}

fn is_cjk_char(&self, c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |     // 中文
        '\u{3400}'..='\u{4DBF}' |     // 中文扩展A
        '\u{20000}'..='\u{2A6DF}' |   // 中文扩展B
        '\u{3040}'..='\u{309F}' |     // 平假名
        '\u{30A0}'..='\u{30FF}' |     // 片假名
        '\u{AC00}'..='\u{D7AF}' |     // 韩文
        '\u{FF00}'..='\u{FFEF}'       // 全角字符
    )
}

fn estimate_ascii_word_tokens(&self, word_len: usize) -> usize {
    // 对于ASCII单词，使用更精确的估算
    match word_len {
        0 => 0,
        1..=3 => 1,  // 短单词通常1个token
        4..=7 => 2,  // 中等长度单词
        8..=11 => 3,
        _ => word_len.div_ceil(4), // 长单词按平均估算
    }
}

fn estimate_messages_tokens(&self, messages: &[AIMessage]) -> usize {
    let mut total = 0;
    
    for msg in messages {
        // 不同角色的基础token数不同
        let role_tokens = match msg.role.as_str() {
            "system" => 4,    // "system"通常4个token
            "user" => 3,      // "user"通常3个token  
            "assistant" => 4, // "assistant"通常4个token
            _ => 3,
        };
        
        let content_tokens = self.estimate_tokens(&msg.content);
        
        // 考虑消息格式（不同模型格式不同）
        let format_tokens = match msg.role.as_str() {
            "system" => 4,    // 系统消息额外格式token
            "user" => 3,      // 用户消息格式
            "assistant" => 3, // 助手消息格式
            _ => 3,
        };
        
        total += role_tokens + content_tokens + format_tokens;
    }
    
    // 添加对话开始和结束的token
    total + 2
}

    /// 裁剪历史消息，确保总token数不超过模型限制
    fn trim_history(&mut self) {
        // 模型最大token限制（默认4096）
        const MAX_TOKENS: usize = 4096;

        // 首先按消息数量裁剪
        if self.messages.len() > self.max_history_length {
            // 保留系统消息（如果有）
            let mut new_messages = Vec::new();
            if let Some(first_message) = self.messages.first()
                && first_message.role == MessageRole::System.to_string() {
                    new_messages.push(first_message.clone());
                }

            // 保留最新的消息
            let start_index = self.messages.len() - (self.max_history_length - new_messages.len());
            new_messages.extend_from_slice(&self.messages[start_index..]);

            self.messages = new_messages;
        }

        // 然后按token数量裁剪
        // 保留系统消息（如果有）
        let has_system_message = self
            .messages
            .first()
            .map(|msg| msg.role == MessageRole::System.to_string())
            .unwrap_or(false);

        let mut system_message = None;
        let mut user_assistant_messages = Vec::new();

        if has_system_message {
            system_message = self.messages.first().cloned();
            user_assistant_messages.extend_from_slice(&self.messages[1..]);
        } else {
            user_assistant_messages.extend_from_slice(&self.messages);
        }

        // 计算系统消息的token数
        let system_tokens = system_message
            .as_ref()
            .map(|msg| self.estimate_tokens(&msg.content) + 5) // 5个token用于系统消息格式
            .unwrap_or(0);

        // 从最新的消息开始，向前添加，直到总token数接近限制
        let mut trimmed_messages = Vec::new();
        let mut total_tokens = system_tokens;

        // 从最新的消息开始遍历
        for msg in user_assistant_messages.iter().rev() {
            let msg_tokens = self.estimate_tokens(&msg.content) + 5; // 5个token用于消息格式

            // 如果添加这个消息不会超过限制，就添加它
            if total_tokens + msg_tokens <= MAX_TOKENS {
                trimmed_messages.push(msg.clone());
                total_tokens += msg_tokens;
            } else {
                // 否则停止添加
                break;
            }
        }

        // 反转消息顺序，因为我们是从后往前添加的
        trimmed_messages.reverse();

        // 重新构建消息列表
        let mut new_messages = Vec::new();
        if let Some(msg) = system_message {
            new_messages.push(msg);
        }
        new_messages.extend(trimmed_messages);

        self.messages = new_messages;
    }
}
