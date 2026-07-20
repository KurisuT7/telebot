use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use grammers_client::Client;
use grammers_client::media::Media;
use grammers_client::message::Message;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::config::AiConfig;
use crate::plugin::{CommandContext, Plugin};
use crate::store::Store;
use crate::telegram::{
    ai_rich_response, edit_progress, replace_with_chunks, replace_with_markdown,
    replace_with_rich_chunks,
};

const MAX_REPLY_CONTEXT_CHARS: usize = 8_000;
const MAX_AI_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_AI_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AI_IMAGE_TOTAL_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone, Debug)]
struct AiImage {
    mime_type: String,
    base64_data: String,
    byte_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GeminiSource {
    title: String,
    url: String,
}

#[derive(Debug)]
struct GeminiAnswer {
    text: String,
    sources: Vec<GeminiSource>,
    model: String,
    search_calls: usize,
}

pub async fn check_provider(config: &AiConfig, api_key: String) -> Result<()> {
    if !config.enabled {
        bail!("AI plugin is disabled");
    }
    let provider = GeminiProvider::new(config, api_key)?;
    let started = Instant::now();
    let answer = provider
        .generate_search_hedged(
            "Windows 11 的 ms-cxh:localonly 命令有什么作用？请用一句中文回答。",
            &[],
            Duration::from_secs(config.search_timeout_seconds),
            Duration::from_secs(config.search_hedge_seconds),
        )
        .await?;
    if answer.text.trim().is_empty() {
        bail!("AI provider returned an empty response");
    }
    println!(
        "AI native search check passed in {} ms (model={}, calls={}, sources={}, answer={} chars)",
        started.elapsed().as_millis(),
        answer.model,
        answer.search_calls,
        answer.sources.len(),
        answer.text.chars().count()
    );
    Ok(())
}

pub struct AiPlugin {
    config: AiConfig,
    provider: GeminiProvider,
    store: Arc<Store>,
    capacity: Arc<Semaphore>,
}

impl AiPlugin {
    pub fn new(config: AiConfig, api_key: String, store: Arc<Store>) -> Result<Self> {
        let provider = GeminiProvider::new(&config, api_key)?;
        Ok(Self {
            capacity: Arc::new(Semaphore::new(config.max_concurrent)),
            config,
            provider,
            store,
        })
    }

    async fn answer(
        &self,
        context: &CommandContext,
        question: String,
        prompt: String,
        use_search: bool,
        images: Vec<AiImage>,
    ) -> Result<()> {
        let scope = context.message.peer_id().to_string();
        let _capacity = self
            .capacity
            .acquire()
            .await
            .map_err(|_| anyhow!("AI worker pool is shutting down"))?;
        // Normal AI requests are intentionally stateless. The only context comes from the
        // message explicitly replied to by this command.
        let started = Instant::now();
        let (answer, searched) = if use_search {
            self.answer_with_native_search(&prompt, &images, &scope)
                .await?
        } else {
            (
                self.provider
                    .generate_chat_with_timeout(
                        &prompt,
                        &images,
                        Duration::from_secs(self.config.fallback_timeout_seconds),
                    )
                    .await?,
                false,
            )
        };

        let rich = ai_rich_response(
            &question,
            &answer,
            "Gemini",
            if self.config.collapse_long_messages {
                self.config.collapse_threshold_chars
            } else {
                usize::MAX
            },
        );
        replace_with_rich_chunks(&context.client, &context.message, &rich).await?;
        info!(
            scope,
            searched,
            elapsed_ms = started.elapsed().as_millis(),
            "AI request completed"
        );
        Ok(())
    }

    async fn answer_with_native_search(
        &self,
        prompt: &str,
        images: &[AiImage],
        scope: &str,
    ) -> Result<(String, bool)> {
        match self
            .provider
            .generate_search_hedged(
                prompt,
                images,
                Duration::from_secs(self.config.search_timeout_seconds),
                Duration::from_secs(self.config.search_hedge_seconds),
            )
            .await
        {
            Ok(mut answer) => {
                info!(
                    scope,
                    model = answer.model,
                    search_calls = answer.search_calls,
                    sources = answer.sources.len(),
                    "Gemini native search completed"
                );
                append_sources(&mut answer.text, &answer.sources);
                Ok((answer.text, true))
            }
            Err(search_error) => {
                warn!(error = %search_error, scope, "Gemini native search models failed; using non-search fallback");
                let fallback = self
                    .provider
                    .generate_chat_with_timeout(
                        prompt,
                        images,
                        Duration::from_secs(self.config.fallback_timeout_seconds),
                    )
                    .await
                    .map_err(|fallback_error| {
                        both_ai_paths_failed(&search_error, &fallback_error, scope)
                    })?;
                Ok((offline_fallback_answer(fallback), false))
            }
        }
    }

    fn status_text(&self) -> String {
        format!(
            "🤖 **telebot AI**\n\n- 服务商：`Gemini`\n- 主模型：`{}`\n- 原生搜索备用模型：`{}`\n- 原生搜索：`Google Search / Interactions API`\n- 思考等级：`{}`\n- 裸命令默认搜索：**{}**\n- 自动记忆上下文：**关闭**\n- 回复消息：仅作为本次请求上下文\n- 长文折叠：{}（{} 字起）\n- 原生搜索总时限：{} 秒\n- 慢请求切备用模型：{} 秒\n- 无搜索兜底时限：{} 秒\n- 并发上限：{}",
            self.config.model,
            self.config.search_fallback_model,
            self.config.thinking_level,
            if self.config.default_search {
                "开启"
            } else {
                "关闭"
            },
            if self.config.collapse_long_messages {
                "开启"
            } else {
                "关闭"
            },
            self.config.collapse_threshold_chars,
            self.config.search_timeout_seconds,
            self.config.search_hedge_seconds,
            self.config.fallback_timeout_seconds,
            self.config.max_concurrent,
        )
    }

    async fn show_help(&self, context: &CommandContext) -> Result<()> {
        replace_with_markdown(
            &context.client,
            &context.message,
            "# 🤖 telebot AI\n\n- `.ai <问题>` — 默认联网搜索\n- `.ai` — 回复消息时直接分析被回复内容\n- `.ai search <问题>` — 强制联网搜索\n- `.ai chat <问题>` — 不联网回答\n- `.ai status` — 查看当前配置\n- `.ai reset` — 清除旧版本遗留上下文\n\n> 默认不保存对话历史；只有你明确回复的消息会作为本次上下文。",
        )
        .await
    }
}

#[async_trait]
impl Plugin for AiPlugin {
    fn name(&self) -> &'static str {
        "ai"
    }
    fn commands(&self) -> &'static [&'static str] {
        &["ai"]
    }

    async fn handle(&self, context: CommandContext) -> Result<()> {
        let raw = context.command.raw_args.trim();
        if raw.eq_ignore_ascii_case("help") || raw == "?" {
            return self.show_help(&context).await;
        }

        if raw.eq_ignore_ascii_case("status") || raw.eq_ignore_ascii_case("config") {
            return replace_with_markdown(&context.client, &context.message, &self.status_text())
                .await;
        }

        if raw.eq_ignore_ascii_case("reset") || raw.eq_ignore_ascii_case("clear") {
            let deleted = self
                .store
                .clear_history(&context.message.peer_id().to_string())
                .await?;
            return replace_with_chunks(
                &context.client,
                &context.message,
                &format!(
                    "✅ 已清除旧版本遗留上下文（{deleted} 条记录）\n\n当前版本默认不保存上下文。"
                ),
            )
            .await;
        }

        let (first, rest) = split_first(raw);
        let (use_search, explicit_question) = match first.to_ascii_lowercase().as_str() {
            "search" | "s" => (true, rest.to_owned()),
            "chat" | "c" => (false, rest.to_owned()),
            _ => (self.config.default_search, raw.to_owned()),
        };
        let preparation_started = Instant::now();
        edit_progress(
            &context.message,
            if use_search {
                "🔎 正在联网搜索…"
            } else {
                "💭 正在思考…"
            },
        )
        .await?;
        info!(
            message_id = context.message.id(),
            elapsed_ms = preparation_started.elapsed().as_millis(),
            "AI progress displayed"
        );

        let replied = context.message.get_reply().await?;
        let reply_text = replied
            .as_ref()
            .map(|message| reply_context_text(&context.message, message))
            .unwrap_or_default();
        let images = collect_ai_images(&context.client, &context.message, replied.as_ref()).await?;

        if raw.is_empty() && reply_text.trim().is_empty() && images.is_empty() {
            return self.show_help(&context).await;
        }

        let question = if explicit_question.trim().is_empty() {
            if reply_text.trim().is_empty() && !images.is_empty() {
                "请分析图片内容。".to_owned()
            } else {
                reply_text.clone()
            }
        } else {
            explicit_question
        };
        if question.trim().is_empty() {
            bail!("请在 .ai 后输入问题，或回复一条文字消息后使用 .ai");
        }

        let prompt = if !reply_text.trim().is_empty() && reply_text.trim() != question.trim() {
            format!("上下文：\n{reply_text}\n\n问题：\n{question}")
        } else {
            question.clone()
        };
        info!(
            message_id = context.message.id(),
            elapsed_ms = preparation_started.elapsed().as_millis(),
            images = images.len(),
            "AI request prepared"
        );
        self.answer(&context, question, prompt, use_search, images)
            .await
    }
}

fn offline_fallback_answer(answer: String) -> String {
    format!("⚠️ 联网检索暂时不可用，以下为无联网回答：\n\n{answer}")
}

fn both_ai_paths_failed(
    search_error: &anyhow::Error,
    fallback_error: &anyhow::Error,
    scope: &str,
) -> anyhow::Error {
    warn!(
        search_error = %search_error,
        fallback_error = %fallback_error,
        scope,
        "AI search and non-search fallback both failed"
    );
    anyhow!("AI 服务暂时没有返回有效内容，请稍后重试")
}

fn append_sources(answer: &mut String, sources: &[GeminiSource]) {
    if sources.is_empty() {
        return;
    }
    answer.push_str("\n\n### 🔗 来源");
    for source in sources.iter().take(6) {
        let title = take_chars(&source.title, 120).replace(['[', ']'], "");
        let url = source.url.replace(')', "%29");
        answer.push_str(&format!("\n- [{title}]({url})"));
    }
}

fn take_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn reply_context_text(
    command: &grammers_client::message::Message,
    replied: &grammers_client::message::Message,
) -> String {
    let selected = command.reply_header().and_then(|header| match header {
        grammers_tl_types::enums::MessageReplyHeader::Header(value)
            if value.quote && value.quote_text.is_some() =>
        {
            value.quote_text
        }
        _ => None,
    });
    truncate_chars(
        selected.as_deref().unwrap_or(replied.text()),
        MAX_REPLY_CONTEXT_CHARS,
    )
}

async fn collect_ai_images(
    client: &Client,
    command: &Message,
    replied: Option<&Message>,
) -> Result<Vec<AiImage>> {
    let mut images = Vec::new();
    if let Some(message) = replied
        && let Some(image) = download_ai_image(client, message, "reply").await?
    {
        images.push(image);
    }
    if let Some(image) = download_ai_image(client, command, "command").await? {
        images.push(image);
    }
    let total = images.iter().map(|image| image.byte_len).sum::<usize>();
    if total > MAX_AI_IMAGE_TOTAL_BYTES {
        bail!("图片总大小超过 12 MB，请减少图片后重试");
    }
    Ok(images)
}

async fn download_ai_image(
    client: &Client,
    message: &Message,
    label: &str,
) -> Result<Option<AiImage>> {
    let Some(media) = message.media() else {
        return Ok(None);
    };
    let temporary = tempfile::tempdir().context("failed to create AI image temporary directory")?;
    let path = temporary.path().join(format!("{label}.bin"));
    let mime_type = match &media {
        Media::Photo(photo) => {
            client
                .download_media(photo, &path)
                .await
                .context("下载回复图片失败")?;
            "image/jpeg".to_owned()
        }
        Media::Sticker(sticker) => {
            let mime = sticker.document.mime_type().unwrap_or("image/webp");
            if !matches!(mime, "image/jpeg" | "image/png" | "image/webp") {
                return Ok(None);
            }
            client
                .download_media(&sticker.document, &path)
                .await
                .context("下载回复贴纸失败")?;
            mime.to_owned()
        }
        Media::Document(document) => {
            let mime = document.mime_type().unwrap_or("application/octet-stream");
            if !matches!(mime, "image/jpeg" | "image/png" | "image/webp") {
                return Ok(None);
            }
            client
                .download_media(document, &path)
                .await
                .context("下载回复图片文件失败")?;
            mime.to_owned()
        }
        _ => return Ok(None),
    };
    let metadata = tokio::fs::metadata(&path).await?;
    if metadata.len() == 0 {
        bail!("回复图片为空");
    }
    if metadata.len() > MAX_AI_IMAGE_BYTES {
        bail!("单张图片超过 8 MB，请压缩后重试");
    }
    let bytes = tokio::fs::read(&path).await?;
    Ok(Some(AiImage {
        mime_type,
        base64_data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        byte_len: bytes.len(),
    }))
}

fn split_first(input: &str) -> (&str, &str) {
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    (&input[..end], input[end..].trim())
}

fn truncate_chars(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.to_owned()
    } else {
        let mut value = input.chars().take(max).collect::<String>();
        value.push_str("\n…（被回复消息已截断）");
        value
    }
}

struct GeminiProvider {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    system_prompt: String,
    primary_model: String,
    search_fallback_model: String,
    thinking_level: String,
    max_output_tokens: u32,
}

impl GeminiProvider {
    fn new(config: &AiConfig, api_key: String) -> Result<Self> {
        for (name, model) in [
            ("ai.model", config.model.as_str()),
            (
                "ai.search_fallback_model",
                config.search_fallback_model.as_str(),
            ),
        ] {
            if !model
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            {
                bail!("{name} contains unsupported characters");
            }
        }
        let endpoint = format!(
            "{}/v1beta/interactions",
            config.base_url.trim_end_matches('/')
        );
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .user_agent(concat!("telebot/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to construct AI HTTP client")?;
        Ok(Self {
            http,
            endpoint,
            api_key,
            system_prompt: config.system_prompt.clone(),
            primary_model: config.model.clone(),
            search_fallback_model: config.search_fallback_model.clone(),
            thinking_level: config.thinking_level.clone(),
            max_output_tokens: config.max_output_tokens,
        })
    }

    async fn generate_search_hedged(
        &self,
        query: &str,
        images: &[AiImage],
        total_limit: Duration,
        hedge_delay: Duration,
    ) -> Result<GeminiAnswer> {
        let started = Instant::now();
        let deadline = started + total_limit;
        let mut primary =
            Box::pin(self.request(&self.primary_model, query, true, images, total_limit));

        let primary_error = match timeout(hedge_delay, primary.as_mut()).await {
            Ok(Ok(answer)) => return Ok(answer),
            Ok(Err(error)) => Some(error),
            Err(_) => {
                info!(
                    model = self.primary_model,
                    hedge_after_ms = hedge_delay.as_millis(),
                    "Gemini native search is slow; starting fallback search model"
                );
                None
            }
        };

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(500) {
            return Err(anyhow!(
                "Gemini native search deadline exceeded after {:.1}s",
                started.elapsed().as_secs_f32()
            ));
        }
        let mut fallback =
            Box::pin(self.request(&self.search_fallback_model, query, true, images, remaining));

        if let Some(primary_error) = primary_error {
            return fallback.await.map_err(|fallback_error| {
                native_search_models_failed(&primary_error, &fallback_error, started.elapsed())
            });
        }

        tokio::select! {
            primary_result = &mut primary => match primary_result {
                Ok(answer) => Ok(answer),
                Err(primary_error) => match fallback.await {
                    Ok(answer) => Ok(answer),
                    Err(fallback_error) => Err(native_search_models_failed(
                        &primary_error,
                        &fallback_error,
                        started.elapsed(),
                    )),
                },
            },
            fallback_result = &mut fallback => match fallback_result {
                Ok(answer) => Ok(answer),
                Err(fallback_error) => match primary.await {
                    Ok(answer) => Ok(answer),
                    Err(primary_error) => Err(native_search_models_failed(
                        &primary_error,
                        &fallback_error,
                        started.elapsed(),
                    )),
                },
            },
        }
    }

    async fn generate_chat_with_timeout(
        &self,
        query: &str,
        images: &[AiImage],
        limit: Duration,
    ) -> Result<String> {
        let started = Instant::now();
        let deadline = started + limit;
        let mut last_error = None;
        for attempt in 0..2 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining < Duration::from_millis(500) {
                break;
            }
            match self
                .request(&self.primary_model, query, false, images, remaining)
                .await
            {
                Ok(answer) => return Ok(answer.text),
                Err(error) => {
                    let retry = error.transient && attempt == 0;
                    last_error = Some(error);
                    if !retry {
                        break;
                    }
                    sleep(Duration::from_millis(150 + rand::random_range(0..150))).await;
                }
            }
        }
        let error = last_error
            .unwrap_or_else(|| ProviderError::transient("Gemini request deadline exceeded"));
        Err(anyhow!(
            "Gemini request failed after {:.1}s: {error}",
            started.elapsed().as_secs_f32()
        ))
    }

    async fn request(
        &self,
        model: &str,
        query: &str,
        use_search: bool,
        images: &[AiImage],
        limit: Duration,
    ) -> std::result::Result<GeminiAnswer, ProviderError> {
        timeout(limit, self.request_inner(model, query, use_search, images))
            .await
            .map_err(|_| ProviderError::transient("Gemini request timed out"))?
    }

    async fn request_inner(
        &self,
        model: &str,
        query: &str,
        use_search: bool,
        images: &[AiImage],
    ) -> std::result::Result<GeminiAnswer, ProviderError> {
        let mut input = Vec::with_capacity(images.len() + 1);
        for image in images {
            input.push(json!({
                "type": "image",
                "mime_type": image.mime_type,
                "data": image.base64_data,
            }));
        }
        input.push(json!({"type": "text", "text": query}));

        let system_instruction = if use_search {
            format!(
                "{}\n\n本次请求必须先调用 Google Search 获取相关资料，再基于搜索结果回答。",
                self.system_prompt
            )
        } else {
            self.system_prompt.clone()
        };
        let mut body = json!({
            "model": model,
            "input": input,
            "system_instruction": system_instruction,
            "generation_config": {
                "thinking_level": self.thinking_level,
                "max_output_tokens": self.max_output_tokens,
            },
            "store": false,
        });
        if use_search {
            body["tools"] = json!([{"type": "google_search"}]);
        }

        let response = self
            .http
            .post(&self.endpoint)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(ProviderError::from_reqwest)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|size| size > MAX_AI_RESPONSE_BYTES as u64)
        {
            return Err(ProviderError::permanent("Gemini response was too large"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(ProviderError::from_reqwest)?;
        if bytes.len() > MAX_AI_RESPONSE_BYTES {
            return Err(ProviderError::permanent("Gemini response was too large"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            ProviderError::permanent(format!("invalid Gemini JSON response: {error}"))
        })?;

        if !status.is_success() {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Gemini returned an unknown error");
            return Err(ProviderError {
                message: format!("Gemini HTTP {}: {}", status.as_u16(), message),
                transient: is_transient_status(status),
            });
        }

        let steps = value
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::permanent("Gemini interaction did not contain steps"))?;
        let mut text = String::new();
        let mut sources = Vec::new();
        let mut seen_sources = HashSet::new();
        let mut search_calls = 0;
        for step in steps {
            match step.get("type").and_then(Value::as_str) {
                Some("google_search_call") => search_calls += 1,
                Some("model_output") => {
                    let Some(content) = step.get("content").and_then(Value::as_array) else {
                        continue;
                    };
                    for block in content {
                        if block.get("type").and_then(Value::as_str) != Some("text") {
                            continue;
                        }
                        if let Some(value) = block.get("text").and_then(Value::as_str) {
                            text.push_str(value);
                        }
                        let annotations = block
                            .get("annotations")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten();
                        for annotation in annotations {
                            if annotation.get("type").and_then(Value::as_str)
                                != Some("url_citation")
                            {
                                continue;
                            }
                            let Some(url) = annotation.get("url").and_then(Value::as_str) else {
                                continue;
                            };
                            if !url.starts_with("https://") || !seen_sources.insert(url.to_owned())
                            {
                                continue;
                            }
                            let title = annotation
                                .get("title")
                                .and_then(Value::as_str)
                                .filter(|title| !title.trim().is_empty())
                                .unwrap_or("参考资料");
                            sources.push(GeminiSource {
                                title: title.to_owned(),
                                url: url.to_owned(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        if text.trim().is_empty() {
            return Err(empty_interaction_error(&value));
        }
        if use_search && search_calls == 0 {
            return Err(ProviderError::transient(
                "Gemini did not call native Google Search",
            ));
        }

        Ok(GeminiAnswer {
            text,
            sources,
            model: model.to_owned(),
            search_calls,
        })
    }
}

fn native_search_models_failed(
    primary: &ProviderError,
    fallback: &ProviderError,
    elapsed: Duration,
) -> anyhow::Error {
    anyhow!(
        "Gemini native search failed after {:.1}s; primary: {}; fallback: {}",
        elapsed.as_secs_f32(),
        primary,
        fallback
    )
}

fn empty_interaction_error(value: &Value) -> ProviderError {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    let step_types = value
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|step| step.get("type").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(",");
    ProviderError::transient(format!(
        "Gemini returned an empty interaction (status={status}, steps={step_types})"
    ))
}

#[derive(Debug)]
struct ProviderError {
    message: String,
    transient: bool,
}

impl ProviderError {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }
    fn from_reqwest(error: reqwest::Error) -> Self {
        Self {
            transient: error.is_timeout() || error.is_connect() || error.is_request(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

fn is_transient_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_interaction_is_retryable_and_keeps_step_metadata() {
        let value = json!({
            "status": "completed",
            "steps": [{"type": "google_search_call"}, {"type": "thought"}]
        });
        let error = empty_interaction_error(&value);
        assert!(error.transient);
        assert!(error.message.contains("status=completed"));
        assert!(error.message.contains("google_search_call,thought"));
    }

    #[test]
    fn native_sources_are_appended_and_sanitized() {
        let sources = vec![GeminiSource {
            title: "Windows local account [guide]".to_owned(),
            url: "https://example.com/guide".to_owned(),
        }];
        let mut answer = "回答".to_owned();
        append_sources(&mut answer, &sources);
        assert!(answer.contains("### 🔗 来源"));
        assert!(answer.contains("[Windows local account guide]"));
    }
}
