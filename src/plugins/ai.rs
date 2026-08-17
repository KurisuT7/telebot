use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use grammers_client::Client;
use grammers_client::media::{Media, Photo, PhotoSize};
use grammers_client::message::Message;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};
use tokio::sync::{RwLock, Semaphore};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::config::{AiApiFormat, AiConfig, Config, MessagesConfig};
use crate::plugin::{CommandContext, Plugin};
use crate::store::{AiHistoryEntry, Store};
use crate::telegram::{
    ai_rich_response, edit_progress, replace_with_chunks, replace_with_markdown,
    replace_with_rich_chunks,
};

const MAX_REPLY_CONTEXT_CHARS: usize = 8_000;
const MAX_AI_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_AI_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AI_IMAGE_TOTAL_BYTES: usize = 12 * 1024 * 1024;
const MAX_HISTORY_ITEM_CHARS: usize = 4_000;
const MAX_HISTORY_TOTAL_CHARS: usize = 24_000;
const DEFAULT_CONTEXT_TURNS: usize = 6;
const AI_SETTING_PREFIX: &str = "ai.runtime.";
const AI_PROVIDER_SETTING: &str = "ai.runtime.provider";
const AI_API_FORMAT_SETTING: &str = "ai.runtime.api_format";
const AI_BASE_URL_SETTING: &str = "ai.runtime.base_url";
const AI_KEY_SETTING: &str = "ai.runtime.api_key";
const AI_MODEL_SETTING: &str = "ai.runtime.model";
const AI_SEARCH_MODEL_SETTING: &str = "ai.runtime.search_model";
const AI_CONTEXT_SETTING: &str = "ai.runtime.context_turns";
const AI_THINKING_SETTING: &str = "ai.runtime.thinking_level";
const AI_DEFAULT_SEARCH_SETTING: &str = "ai.runtime.default_search";
const AI_SYSTEM_PROMPT_SETTING: &str = "ai.runtime.system_prompt";
const AI_MAX_OUTPUT_TOKENS_SETTING: &str = "ai.runtime.max_output_tokens";
const AI_SEARCH_TIMEOUT_SETTING: &str = "ai.runtime.search_timeout_seconds";
const AI_IMAGE_SEARCH_TIMEOUT_SETTING: &str = "ai.runtime.image_search_timeout_seconds";
const AI_SEARCH_HEDGE_SETTING: &str = "ai.runtime.search_hedge_seconds";
const AI_FALLBACK_TIMEOUT_SETTING: &str = "ai.runtime.fallback_timeout_seconds";
const AI_COLLAPSE_SETTING: &str = "ai.runtime.collapse_long_messages";
const AI_SEARCHING_MESSAGE_SETTING: &str = "ai.runtime.message.searching";
const AI_THINKING_MESSAGE_SETTING: &str = "ai.runtime.message.thinking";

#[derive(Clone, Debug)]
pub struct AiProgressConfig {
    pub default_search: bool,
    pub searching: String,
    pub thinking: String,
}

impl AiProgressConfig {
    pub fn new(config: &AiConfig, messages: &MessagesConfig) -> Self {
        Self {
            default_search: config.default_search,
            searching: messages.ai_searching.clone(),
            thinking: messages.ai_thinking.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct AiImage {
    mime_type: String,
    base64_data: String,
    byte_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AiSource {
    title: String,
    url: String,
}

#[derive(Debug)]
struct AiAnswer {
    text: String,
    sources: Vec<AiSource>,
    model: String,
    search_calls: usize,
}

#[derive(Clone)]
struct AiRuntimeOptions {
    provider_name: String,
    api_format: AiApiFormat,
    base_url: String,
    api_key: String,
    primary_model: String,
    search_fallback_model: String,
    context_turns: usize,
    key_overridden: bool,
    config: AiConfig,
    messages: MessagesConfig,
}

#[derive(Clone)]
struct AiRuntime {
    provider_name: String,
    api_format: AiApiFormat,
    base_url: String,
    api_key: String,
    primary_model: String,
    search_fallback_model: String,
    context_turns: usize,
    key_overridden: bool,
    config: AiConfig,
    messages: MessagesConfig,
    provider: Arc<dyn AiProviderBackend>,
}

impl From<&AiRuntime> for AiRuntimeOptions {
    fn from(runtime: &AiRuntime) -> Self {
        Self {
            provider_name: runtime.provider_name.clone(),
            api_format: runtime.api_format,
            base_url: runtime.base_url.clone(),
            api_key: runtime.api_key.clone(),
            primary_model: runtime.primary_model.clone(),
            search_fallback_model: runtime.search_fallback_model.clone(),
            context_turns: runtime.context_turns,
            key_overridden: runtime.key_overridden,
            config: runtime.config.clone(),
            messages: runtime.messages.clone(),
        }
    }
}

impl AiRuntime {
    fn build(options: AiRuntimeOptions) -> Result<Self> {
        let AiRuntimeOptions {
            provider_name,
            api_format,
            base_url,
            api_key,
            primary_model,
            search_fallback_model,
            context_turns,
            key_overridden,
            config,
            messages,
        } = options;
        validate_provider_name(&provider_name)?;
        let base_url = normalize_base_url(&base_url)?;
        validate_api_key(&api_key)?;
        validate_model("主模型", &primary_model)?;
        let search_fallback_model = if search_fallback_model.trim().is_empty()
            && api_format == AiApiFormat::OpenaiChatCompletions
        {
            primary_model.clone()
        } else {
            validate_model("搜索备用模型", &search_fallback_model)?;
            search_fallback_model
        };
        if context_turns > 20 {
            bail!("上下文轮数必须在 0 到 20 之间");
        }
        validate_runtime_config(&config, &messages)?;
        let provider = build_provider(
            api_format,
            &config,
            &base_url,
            &primary_model,
            &search_fallback_model,
            api_key.clone(),
        )?;
        Ok(Self {
            provider_name,
            api_format,
            base_url,
            api_key,
            primary_model,
            search_fallback_model,
            context_turns,
            key_overridden,
            config,
            messages,
            provider,
        })
    }
}

fn default_provider_name(config: &AiConfig) -> String {
    if config.provider.eq_ignore_ascii_case("gemini") {
        "Gemini".to_owned()
    } else {
        config.provider.clone()
    }
}

pub async fn check_provider(config: &AiConfig, api_key: String) -> Result<()> {
    if !config.enabled {
        bail!("AI plugin is disabled");
    }
    let provider = build_provider(
        config.api_format,
        config,
        &config.base_url,
        &config.model,
        &config.search_fallback_model,
        api_key,
    )?;
    let started = Instant::now();
    let query = "Windows 11 的 ms-cxh:localonly 命令有什么作用？请用一句中文回答。";
    let (answer, searched) = if provider.supports_native_search() {
        (
            provider
                .generate_search_hedged(
                    query,
                    &[],
                    Duration::from_secs(config.search_timeout_seconds),
                    Duration::from_secs(config.search_hedge_seconds),
                )
                .await?,
            true,
        )
    } else {
        (
            AiAnswer {
                text: provider
                    .generate_chat_with_timeout(
                        query,
                        &[],
                        Duration::from_secs(config.fallback_timeout_seconds),
                    )
                    .await?,
                sources: Vec::new(),
                model: config.model.clone(),
                search_calls: 0,
            },
            false,
        )
    };
    if answer.text.trim().is_empty() {
        bail!("AI provider returned an empty response");
    }
    println!(
        "AI check passed in {} ms (format={}, searched={}, model={}, calls={}, sources={}, answer={} chars)",
        started.elapsed().as_millis(),
        config.api_format.as_str(),
        searched,
        answer.model,
        answer.search_calls,
        answer.sources.len(),
        answer.text.chars().count()
    );
    Ok(())
}

pub struct AiPlugin {
    config_path: PathBuf,
    defaults: RwLock<AiDefaults>,
    runtime: RwLock<AiRuntime>,
    progress: Arc<RwLock<AiProgressConfig>>,
    store: Arc<Store>,
    capacity: Arc<Semaphore>,
}

#[derive(Clone)]
struct AiDefaults {
    config: AiConfig,
    messages: MessagesConfig,
    env_api_key: String,
}

impl AiPlugin {
    pub async fn new(
        config_path: PathBuf,
        config: AiConfig,
        messages: MessagesConfig,
        env_api_key: String,
        store: Arc<Store>,
        progress: Arc<RwLock<AiProgressConfig>>,
    ) -> Result<Self> {
        let defaults = AiDefaults {
            config,
            messages,
            env_api_key,
        };
        let runtime = runtime_from_store(&defaults, &store).await?;
        *progress.write().await = progress_from_runtime(&runtime);
        let max_concurrent = defaults.config.max_concurrent;
        Ok(Self {
            capacity: Arc::new(Semaphore::new(max_concurrent)),
            config_path,
            defaults: RwLock::new(defaults),
            runtime: RwLock::new(runtime),
            progress,
            store,
        })
    }

    async fn install_runtime(&self, runtime: AiRuntime) {
        *self.progress.write().await = progress_from_runtime(&runtime);
        *self.runtime.write().await = runtime;
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
        let runtime = self.runtime.read().await.clone();
        let history = self
            .store
            .ai_history(&scope, runtime.context_turns.saturating_mul(2))
            .await?;
        let prompt = compose_history_prompt(&history, &prompt);
        let started = Instant::now();
        let (answer, searched) = if use_search {
            self.answer_with_native_search(&runtime, &prompt, &images, &scope)
                .await?
        } else {
            (
                runtime
                    .provider
                    .generate_chat_with_timeout(
                        &prompt,
                        &images,
                        Duration::from_secs(runtime.config.fallback_timeout_seconds),
                    )
                    .await?,
                false,
            )
        };

        let rich = ai_rich_response(
            &question,
            &answer,
            &runtime.provider_name,
            runtime.config.collapse_long_messages,
        );
        replace_with_rich_chunks(&context.client, &context.message, &rich).await?;
        if runtime.context_turns > 0
            && let Err(error) = self
                .store
                .append_ai_turn(
                    &scope,
                    &truncate_history_item(&question),
                    &truncate_history_item(&answer),
                    runtime.context_turns,
                )
                .await
        {
            warn!(%error, scope, "failed to persist AI context");
        }
        info!(
            scope,
            provider = runtime.provider_name,
            searched,
            context_turns = runtime.context_turns,
            elapsed_ms = started.elapsed().as_millis(),
            "AI request completed"
        );
        Ok(())
    }

    async fn answer_with_native_search(
        &self,
        runtime: &AiRuntime,
        prompt: &str,
        images: &[AiImage],
        scope: &str,
    ) -> Result<(String, bool)> {
        let search_timeout_seconds = effective_search_timeout_seconds(&runtime.config, images);
        match runtime
            .provider
            .generate_search_hedged(
                prompt,
                images,
                Duration::from_secs(search_timeout_seconds),
                Duration::from_secs(runtime.config.search_hedge_seconds),
            )
            .await
        {
            Ok(mut answer) => {
                info!(
                    scope,
                    model = answer.model,
                    search_calls = answer.search_calls,
                    sources = answer.sources.len(),
                    "AI native search completed"
                );
                append_sources(&mut answer.text, &answer.sources);
                Ok((answer.text, true))
            }
            Err(search_error) => {
                warn!(error = %search_error, scope, "AI native search failed; using non-search fallback");
                let fallback = runtime
                    .provider
                    .generate_chat_with_timeout(
                        prompt,
                        images,
                        Duration::from_secs(runtime.config.fallback_timeout_seconds),
                    )
                    .await
                    .map_err(|fallback_error| {
                        both_ai_paths_failed(&search_error, &fallback_error, scope)
                    })?;
                Ok((offline_fallback_answer(fallback), false))
            }
        }
    }

    async fn status_text(&self) -> String {
        let runtime = self.runtime.read().await;
        let context = if runtime.context_turns == 0 {
            "关闭".to_owned()
        } else {
            format!("开启（{} 轮）", runtime.context_turns)
        };
        format!(
            "🤖 **telebot AI**\n\n- 服务商：`{}`\n- API 格式：`{}`\n- BaseURL：`{}`\n- Key：**{}**\n- 主模型：`{}`\n- 搜索备用模型：`{}`\n- 原生搜索：`{}`\n- 思考等级：`{}`（仅 Gemini Interactions 使用）\n- 裸命令默认搜索：**{}**\n- 自动记忆上下文：**{}**\n- 回复消息：作为本次请求上下文\n- Q/A 引用折叠：{}\n- 纯文字搜索总时限：{} 秒\n- 带图搜索总时限：{} 秒\n- 慢请求切备用模型：{} 秒\n- 无搜索兜底时限：{} 秒\n- 最大输出：{} Token\n- 系统提示词：{} 字\n- 搜索进度：{}\n- 思考进度：{}\n- 并发上限：{}（修改后需重启）",
            runtime.provider_name,
            runtime.api_format.as_str(),
            runtime.base_url,
            if runtime.key_overridden {
                "已设置自定义 Key"
            } else {
                "使用服务器环境变量"
            },
            runtime.primary_model,
            runtime.search_fallback_model,
            native_search_description(runtime.api_format),
            runtime.config.thinking_level,
            if runtime.config.default_search {
                "开启"
            } else {
                "关闭"
            },
            context,
            if runtime.config.collapse_long_messages {
                "开启"
            } else {
                "关闭"
            },
            runtime.config.search_timeout_seconds,
            runtime.config.image_search_timeout_seconds,
            runtime.config.search_hedge_seconds,
            runtime.config.fallback_timeout_seconds,
            runtime.config.max_output_tokens,
            runtime.config.system_prompt.chars().count(),
            runtime.messages.ai_searching,
            runtime.messages.ai_thinking,
            runtime.config.max_concurrent,
        )
    }

    async fn show_help(&self, context: &CommandContext) -> Result<()> {
        let runtime = self.runtime.read().await;
        let default_mode = if runtime.config.default_search {
            "联网搜索"
        } else {
            "普通回答"
        };
        let help = format!(
            "# 🤖 TeleBot AI 帮助\n\n\
## 基本用法\n\n\
- `.ai <问题>` — 按当前默认模式回答；现在默认是 **{default_mode}**。\n\
 - `.ai search <问题>` / `.ai s <问题>` — 强制使用当前 API 格式的原生搜索；Chat Completions 不提供标准搜索。\n\
- `.ai chat <问题>` / `.ai c <问题>` — 强制不联网，直接使用模型回答。\n\
- `.ai status` / `.ai config` — 查看当前生效配置，不显示 Key 内容。\n\
- `.ai help` / `.ai ?` — 显示本帮助。\n\n\
## 回复消息与图片\n\n\
- 回复一条文字消息再发送 `.ai`：把被回复文字作为问题。\n\
 - 回复图片、静态贴纸或带图消息：图片会按当前 API 格式一并提交。\n\
- 回复图片后发送 `.ai <问题>`：结合图片和你的问题联网搜索。\n\
- 回复图片后发送 `.ai chat <问题>`：结合图片回答，但不联网。\n\
- 最多处理 4 张图片；单张不超过 8 MiB，总计不超过 12 MiB。\n\n\
## 上下文记忆\n\n\
- `.ai context` — 查看当前聊天的上下文设置。\n\
- `.ai context on` — 开启默认 6 轮上下文。\n\
- `.ai context <1-20>` — 设置保留轮数。\n\
- `.ai context off` — 停止继续使用上下文。\n\
- `.ai reset` — 清除当前聊天已经保存的上下文。\n\n\
## 动态配置（仅限收藏夹）\n\n\
 - `.ai config provider <API格式> <名称> <BaseURL>` — 更换 API 格式、显示名称和端点。\n\
 - API 格式：`gemini_interactions`、`openai_chat_completions`、`openai_responses`。\n\
- `.ai config key <Key>` — 设置并持久化 Key；消息会先被隐藏，Key 不回显。\n\
- `.ai config clear-key` — 改回服务器环境变量中的 Key。\n\
- `.ai config model <主模型> [搜索备用模型]` — 更换模型。\n\
- `.ai config prompt <系统提示词>` — 修改系统提示词。\n\
- `.ai config thinking <minimal|low|medium|high>` — 修改思考等级。\n\
- `.ai config search <on|off>` — 设置裸 `.ai` 是否默认联网。\n\
- `.ai config timeout <文字秒> <图片秒> <切备用秒> <兜底秒>` — 修改四类时限。\n\
- `.ai config tokens <1-65536>` — 修改最大输出 Token。\n\
- `.ai config collapse <on|off>` — 设置 Q/A 是否使用可折叠引用。\n\
- `.ai config message searching <文案>` — 修改搜索进度文案。\n\
- `.ai config message thinking <文案>` — 修改思考进度文案。\n\
- `.ai config reload` — 重新读取服务器 TOML；SQLite 动态覆盖仍优先。\n\
- `.ai config reset` — 清除全部 AI 动态覆盖，恢复服务器 TOML。\n\n\
## 当前关键参数\n\n\
- 主模型：`{}`\n\
- 搜索备用：`{}`\n\
- 思考等级：`{}`\n\
- 文字/带图搜索时限：`{}` / `{}` 秒\n\
- 搜索进度：{}\n\
- 思考进度：{}\n\n\
> 配置修改会立即生效并保存到本机 SQLite，不需要重新编译。服务商、BaseURL、模型和 Key 会先验证格式；危险或无效值会被拒绝。",
            runtime.primary_model,
            runtime.search_fallback_model,
            runtime.config.thinking_level,
            runtime.config.search_timeout_seconds,
            runtime.config.image_search_timeout_seconds,
            runtime.messages.ai_searching,
            runtime.messages.ai_thinking,
        );
        replace_with_markdown(&context.client, &context.message, &help).await
    }

    async fn ensure_saved_messages(&self, context: &CommandContext) -> Result<()> {
        let me = context.client.get_me().await?;
        if context.message.peer_id() != me.id() {
            bail!("为避免泄露配置，请只在 Telegram 收藏夹中执行 AI 配置修改");
        }
        Ok(())
    }

    async fn context_command(&self, context: &CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            let turns = self.runtime.read().await.context_turns;
            let message = if turns == 0 {
                "🧠 自动上下文：关闭\n设置：.ai context on 或 .ai context <1-20>".to_owned()
            } else {
                format!("🧠 自动上下文：开启（{turns} 轮）\n关闭：.ai context off")
            };
            return replace_with_chunks(&context.client, &context.message, &message).await;
        }
        self.ensure_saved_messages(context).await?;
        if args.len() != 1 {
            bail!("用法：.ai context <0-20|on|off>");
        }
        let turns = match args[0].to_ascii_lowercase().as_str() {
            "on" => DEFAULT_CONTEXT_TURNS,
            "off" => 0,
            value => parse_context_turns(value)?,
        };
        self.store
            .set_setting(AI_CONTEXT_SETTING, &turns.to_string())
            .await?;
        self.runtime.write().await.context_turns = turns;
        let message = if turns == 0 {
            "✅ 自动上下文已关闭；已有记录可用 .ai reset 清除".to_owned()
        } else {
            format!("✅ 自动上下文已设置为 {turns} 轮（按聊天独立保存）")
        };
        replace_with_chunks(&context.client, &context.message, &message).await
    }

    async fn config_command(&self, context: &CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
            return replace_with_markdown(
                &context.client,
                &context.message,
                &self.status_text().await,
            )
            .await;
        }
        let action = args[0].to_ascii_lowercase();
        if action == "context" {
            return self.context_command(context, &args[1..]).await;
        }
        if action == "key" {
            edit_progress(&context.message, "🔐 正在安全更新 AI Key…").await?;
        }
        self.ensure_saved_messages(context).await?;
        match action.as_str() {
            "provider" => {
                if !(3..=4).contains(&args.len()) {
                    bail!("用法：.ai config provider [API格式] <名称> <BaseURL>");
                }
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                let (format, name, base_url) = if args.len() == 4 {
                    (args[1].parse::<AiApiFormat>()?, &args[2], &args[3])
                } else {
                    (current.api_format, &args[1], &args[2])
                };
                options.api_format = format;
                options.provider_name = name.to_owned();
                options.base_url = base_url.to_owned();
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(AI_PROVIDER_SETTING, &updated.provider_name)
                    .await?;
                self.store
                    .set_setting(AI_API_FORMAT_SETTING, updated.api_format.as_str())
                    .await?;
                self.store
                    .set_setting(AI_BASE_URL_SETTING, &updated.base_url)
                    .await?;
                let message = format!(
                    "✅ AI 服务商已更新\n名称：{}\nAPI 格式：{}\nBaseURL：{}",
                    updated.provider_name,
                    updated.api_format.as_str(),
                    updated.base_url
                );
                self.install_runtime(updated).await;
                replace_with_chunks(&context.client, &context.message, &message).await
            }
            "key" => {
                if args.len() != 2 {
                    bail!("用法：.ai config key <Key>");
                }
                validate_api_key(&args[1])?;
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.api_key = args[1].clone();
                options.key_overridden = true;
                let updated = AiRuntime::build(options)?;
                self.store.set_setting(AI_KEY_SETTING, &args[1]).await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    "✅ AI Key 已更新并持久化；不会回显 Key",
                )
                .await
            }
            "clear-key" | "env-key" => {
                let current = self.runtime.read().await.clone();
                let defaults = self.defaults.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.api_key = defaults.env_api_key;
                options.key_overridden = false;
                let updated = AiRuntime::build(options)?;
                self.store.delete_setting(AI_KEY_SETTING).await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    "✅ 已改回服务器环境变量中的 AI Key",
                )
                .await
            }
            "model" => {
                if !(2..=3).contains(&args.len()) {
                    bail!("用法：.ai config model <主模型> [搜索备用模型]");
                }
                let current = self.runtime.read().await.clone();
                let search_model = args
                    .get(2)
                    .cloned()
                    .unwrap_or_else(|| current.search_fallback_model.clone());
                let mut options = AiRuntimeOptions::from(&current);
                options.primary_model = args[1].clone();
                options.search_fallback_model = search_model;
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(AI_MODEL_SETTING, &updated.primary_model)
                    .await?;
                self.store
                    .set_setting(AI_SEARCH_MODEL_SETTING, &updated.search_fallback_model)
                    .await?;
                let message = format!(
                    "✅ AI 模型已更新\n主模型：{}\n搜索备用：{}",
                    updated.primary_model, updated.search_fallback_model
                );
                self.install_runtime(updated).await;
                replace_with_chunks(&context.client, &context.message, &message).await
            }
            "prompt" => {
                if args.len() < 2 {
                    bail!("用法：.ai config prompt <系统提示词>");
                }
                let value = args[1..].join(" ");
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.config.system_prompt = value.clone();
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(AI_SYSTEM_PROMPT_SETTING, &value)
                    .await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    "✅ 系统提示词已更新并立即生效",
                )
                .await
            }
            "thinking" => {
                if args.len() != 2 {
                    bail!("用法：.ai config thinking <minimal|low|medium|high>");
                }
                let value = args[1].to_ascii_lowercase();
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.config.thinking_level = value.clone();
                let updated = AiRuntime::build(options)?;
                self.store.set_setting(AI_THINKING_SETTING, &value).await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    &format!("✅ 思考等级已设置为 {value}"),
                )
                .await
            }
            "search" | "default-search" => {
                if args.len() != 2 {
                    bail!("用法：.ai config search <on|off>");
                }
                let enabled = parse_bool_setting("默认搜索", &args[1])?;
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.config.default_search = enabled;
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(
                        AI_DEFAULT_SEARCH_SETTING,
                        if enabled { "true" } else { "false" },
                    )
                    .await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    if enabled {
                        "✅ 裸 .ai 已设置为默认联网搜索"
                    } else {
                        "✅ 裸 .ai 已设置为默认普通回答"
                    },
                )
                .await
            }
            "timeout" | "timeouts" => {
                if args.len() != 5 {
                    bail!("用法：.ai config timeout <文字秒> <图片秒> <切备用秒> <兜底秒>");
                }
                let text = parse_u64_setting("文字搜索时限", &args[1])?;
                let image = parse_u64_setting("带图搜索时限", &args[2])?;
                let hedge = parse_u64_setting("备用模型启动时限", &args[3])?;
                let fallback = parse_u64_setting("无搜索兜底时限", &args[4])?;
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.config.search_timeout_seconds = text;
                options.config.image_search_timeout_seconds = image;
                options.config.search_hedge_seconds = hedge;
                options.config.fallback_timeout_seconds = fallback;
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(AI_SEARCH_TIMEOUT_SETTING, &text.to_string())
                    .await?;
                self.store
                    .set_setting(AI_IMAGE_SEARCH_TIMEOUT_SETTING, &image.to_string())
                    .await?;
                self.store
                    .set_setting(AI_SEARCH_HEDGE_SETTING, &hedge.to_string())
                    .await?;
                self.store
                    .set_setting(AI_FALLBACK_TIMEOUT_SETTING, &fallback.to_string())
                    .await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    &format!(
                        "✅ AI 时限已更新\n文字：{text} 秒\n图片：{image} 秒\n切备用：{hedge} 秒\n兜底：{fallback} 秒"
                    ),
                )
                .await
            }
            "tokens" | "max-tokens" => {
                if args.len() != 2 {
                    bail!("用法：.ai config tokens <1-65536>");
                }
                let value = parse_u32_setting("最大输出 Token", &args[1])?;
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.config.max_output_tokens = value;
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(AI_MAX_OUTPUT_TOKENS_SETTING, &value.to_string())
                    .await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    &format!("✅ 最大输出已设置为 {value} Token"),
                )
                .await
            }
            "collapse" => {
                if args.len() != 2 {
                    bail!("用法：.ai config collapse <on|off>");
                }
                let enabled = parse_bool_setting("Q/A 引用折叠", &args[1])?;
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                options.config.collapse_long_messages = enabled;
                let updated = AiRuntime::build(options)?;
                self.store
                    .set_setting(AI_COLLAPSE_SETTING, if enabled { "true" } else { "false" })
                    .await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    if enabled {
                        "✅ Q/A 可折叠引用已开启"
                    } else {
                        "✅ Q/A 可折叠引用已关闭"
                    },
                )
                .await
            }
            "message" | "messages" => {
                if args.len() < 3 {
                    bail!("用法：.ai config message <searching|thinking> <文案>");
                }
                let kind = args[1].to_ascii_lowercase();
                let value = args[2..].join(" ");
                let current = self.runtime.read().await.clone();
                let mut options = AiRuntimeOptions::from(&current);
                let setting = match kind.as_str() {
                    "searching" | "search" => {
                        options.messages.ai_searching = value.clone();
                        AI_SEARCHING_MESSAGE_SETTING
                    }
                    "thinking" | "think" => {
                        options.messages.ai_thinking = value.clone();
                        AI_THINKING_MESSAGE_SETTING
                    }
                    _ => bail!("文案名称只能是 searching 或 thinking"),
                };
                let updated = AiRuntime::build(options)?;
                self.store.set_setting(setting, &value).await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    &format!("✅ {kind} 进度文案已更新为：{value}"),
                )
                .await
            }
            "reload" => {
                if args.len() != 1 {
                    bail!("用法：.ai config reload");
                }
                let loaded = Config::load(&self.config_path)?;
                if !loaded.ai.enabled {
                    bail!("服务器配置已关闭 AI；停用插件需要重启服务");
                }
                let secrets = loaded.load_secrets()?;
                let current_defaults = self.defaults.read().await.clone();
                let mut config = loaded.ai;
                config.max_concurrent = current_defaults.config.max_concurrent;
                let defaults = AiDefaults {
                    config,
                    messages: loaded.messages,
                    env_api_key: secrets.ai_api_key.context("AI Key 未配置")?,
                };
                let updated = runtime_from_store(&defaults, &self.store).await?;
                *self.defaults.write().await = defaults;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    "✅ 已重新读取服务器 AI 与文案配置；SQLite 动态覆盖保持优先。并发上限和插件启停仍需重启服务。",
                )
                .await
            }
            "reset" => {
                let defaults = self.defaults.read().await.clone();
                let updated = AiRuntime::build(AiRuntimeOptions {
                    provider_name: default_provider_name(&defaults.config),
                    api_format: defaults.config.api_format,
                    base_url: defaults.config.base_url.clone(),
                    api_key: defaults.env_api_key.clone(),
                    primary_model: defaults.config.model.clone(),
                    search_fallback_model: defaults.config.search_fallback_model.clone(),
                    context_turns: defaults.config.history_turns,
                    key_overridden: false,
                    config: defaults.config,
                    messages: defaults.messages,
                })?;
                self.store.delete_settings_prefix(AI_SETTING_PREFIX).await?;
                self.install_runtime(updated).await;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    "✅ AI 动态配置已清除，已恢复服务器配置",
                )
                .await
            }
            _ => bail!(
                "未知配置项；可用：provider、key、clear-key、model、prompt、thinking、search、timeout、tokens、collapse、message、context、reload、reset"
            ),
        }
    }
}

fn effective_search_timeout_seconds(config: &AiConfig, images: &[AiImage]) -> u64 {
    if images.is_empty() {
        config.search_timeout_seconds
    } else {
        config.image_search_timeout_seconds
    }
}

fn progress_from_runtime(runtime: &AiRuntime) -> AiProgressConfig {
    AiProgressConfig::new(&runtime.config, &runtime.messages)
}

async fn runtime_from_store(defaults: &AiDefaults, store: &Store) -> Result<AiRuntime> {
    let mut config = defaults.config.clone();
    let mut messages = defaults.messages.clone();

    if let Some(value) = store.get_setting(AI_THINKING_SETTING).await? {
        config.thinking_level = value;
    }
    if let Some(value) = store.get_setting(AI_DEFAULT_SEARCH_SETTING).await? {
        config.default_search = parse_bool_setting("默认搜索", &value)?;
    }
    if let Some(value) = store.get_setting(AI_SYSTEM_PROMPT_SETTING).await? {
        config.system_prompt = value;
    }
    if let Some(value) = store.get_setting(AI_MAX_OUTPUT_TOKENS_SETTING).await? {
        config.max_output_tokens = parse_u32_setting("最大输出 Token", &value)?;
    }
    if let Some(value) = store.get_setting(AI_SEARCH_TIMEOUT_SETTING).await? {
        config.search_timeout_seconds = parse_u64_setting("文字搜索时限", &value)?;
    }
    if let Some(value) = store.get_setting(AI_IMAGE_SEARCH_TIMEOUT_SETTING).await? {
        config.image_search_timeout_seconds = parse_u64_setting("带图搜索时限", &value)?;
    }
    if let Some(value) = store.get_setting(AI_SEARCH_HEDGE_SETTING).await? {
        config.search_hedge_seconds = parse_u64_setting("备用模型启动时限", &value)?;
    }
    if let Some(value) = store.get_setting(AI_FALLBACK_TIMEOUT_SETTING).await? {
        config.fallback_timeout_seconds = parse_u64_setting("无搜索兜底时限", &value)?;
    }
    if let Some(value) = store.get_setting(AI_COLLAPSE_SETTING).await? {
        config.collapse_long_messages = parse_bool_setting("Q/A 引用折叠", &value)?;
    }
    if let Some(value) = store.get_setting(AI_SEARCHING_MESSAGE_SETTING).await? {
        messages.ai_searching = value;
    }
    if let Some(value) = store.get_setting(AI_THINKING_MESSAGE_SETTING).await? {
        messages.ai_thinking = value;
    }

    let provider_name = store
        .get_setting(AI_PROVIDER_SETTING)
        .await?
        .unwrap_or_else(|| default_provider_name(&config));
    let api_format = match store.get_setting(AI_API_FORMAT_SETTING).await? {
        Some(value) => value.parse::<AiApiFormat>()?,
        None => config.api_format,
    };
    let base_url = store
        .get_setting(AI_BASE_URL_SETTING)
        .await?
        .unwrap_or_else(|| config.base_url.clone());
    let key_override = store.get_setting(AI_KEY_SETTING).await?;
    let key_overridden = key_override.is_some();
    let api_key = key_override.unwrap_or_else(|| defaults.env_api_key.clone());
    let primary_model = store
        .get_setting(AI_MODEL_SETTING)
        .await?
        .unwrap_or_else(|| config.model.clone());
    let search_fallback_model = store
        .get_setting(AI_SEARCH_MODEL_SETTING)
        .await?
        .unwrap_or_else(|| config.search_fallback_model.clone());
    let context_turns = match store.get_setting(AI_CONTEXT_SETTING).await? {
        Some(value) => parse_context_turns(&value)?,
        None => config.history_turns,
    };

    AiRuntime::build(AiRuntimeOptions {
        provider_name,
        api_format,
        base_url,
        api_key,
        primary_model,
        search_fallback_model,
        context_turns,
        key_overridden,
        config,
        messages,
    })
}

fn validate_runtime_config(config: &AiConfig, messages: &MessagesConfig) -> Result<()> {
    if !matches!(
        config.thinking_level.as_str(),
        "minimal" | "low" | "medium" | "high"
    ) {
        bail!("思考等级只能是 minimal、low、medium 或 high");
    }
    let prompt_length = config.system_prompt.chars().count();
    if config.system_prompt.trim().is_empty() || prompt_length > 16_000 {
        bail!("系统提示词必须为 1 到 16000 个字符");
    }
    if !(1..=65_536).contains(&config.max_output_tokens) {
        bail!("最大输出 Token 必须在 1 到 65536 之间");
    }
    if !(3..=120).contains(&config.search_timeout_seconds)
        || !(3..=120).contains(&config.image_search_timeout_seconds)
        || !(3..=120).contains(&config.fallback_timeout_seconds)
    {
        bail!("AI 时限必须在 3 到 120 秒之间");
    }
    if config.image_search_timeout_seconds < config.search_timeout_seconds {
        bail!("带图搜索时限不能短于文字搜索时限");
    }
    if !(3..config.search_timeout_seconds).contains(&config.search_hedge_seconds) {
        bail!("备用模型启动时限必须至少 3 秒，并且短于文字搜索总时限");
    }
    validate_runtime_message("搜索进度文案", &messages.ai_searching)?;
    validate_runtime_message("思考进度文案", &messages.ai_thinking)?;
    Ok(())
}

fn validate_runtime_message(name: &str, value: &str) -> Result<()> {
    let length = value.chars().count();
    if value.trim().is_empty() || length > 128 || value.contains('\r') || value.contains('\n') {
        bail!("{name}必须是 1 到 128 个字符的单行文本");
    }
    Ok(())
}

fn parse_bool_setting(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "开启" | "开" => Ok(true),
        "0" | "false" | "off" | "no" | "关闭" | "关" => Ok(false),
        _ => bail!("{name}只接受 on/off"),
    }
}

fn parse_u64_setting(name: &str, value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("{name}必须是整数"))
}

fn parse_u32_setting(name: &str, value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .with_context(|| format!("{name}必须是整数"))
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
        let (first, rest) = split_first(raw);
        match first.to_ascii_lowercase().as_str() {
            "help" | "?" => return self.show_help(&context).await,
            "status" => {
                return replace_with_markdown(
                    &context.client,
                    &context.message,
                    &self.status_text().await,
                )
                .await;
            }
            "config" | "cfg" => {
                return self
                    .config_command(&context, &context.command.args[1..])
                    .await;
            }
            "context" | "ctx" => {
                return self
                    .context_command(&context, &context.command.args[1..])
                    .await;
            }
            "reset" | "clear" => {
                let deleted = self
                    .store
                    .clear_history(&context.message.peer_id().to_string())
                    .await?;
                return replace_with_chunks(
                    &context.client,
                    &context.message,
                    &format!("✅ 已清除当前聊天上下文（{deleted} 条记录）"),
                )
                .await;
            }
            _ => {}
        }

        let runtime = self.runtime.read().await.clone();
        let (use_search, explicit_question) = match first.to_ascii_lowercase().as_str() {
            "search" | "s" => (true, rest.to_owned()),
            "chat" | "c" => (false, rest.to_owned()),
            _ => (runtime.config.default_search, raw.to_owned()),
        };
        let preparation_started = Instant::now();
        edit_progress(
            &context.message,
            if use_search {
                &runtime.messages.ai_searching
            } else {
                &runtime.messages.ai_thinking
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

fn validate_provider_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("服务商名称只能包含字母、数字、点、横线和下划线，且不超过 32 字符");
    }
    Ok(())
}

fn normalize_base_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    if value.len() > 256 {
        bail!("BaseURL 过长");
    }
    let parsed = Url::parse(value).context("BaseURL 无效")?;
    let loopback = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    });
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        bail!("BaseURL 必须使用 HTTPS；只有本机回环地址可使用 HTTP");
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("BaseURL 不能包含账号、查询参数或片段");
    }
    Ok(value.to_owned())
}

fn validate_api_key(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_whitespace) {
        bail!("AI Key 不能为空、不能包含空白，且不能超过 1024 字符");
    }
    Ok(())
}

fn validate_model(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
    {
        bail!("{label}名称包含不支持的字符");
    }
    Ok(())
}

fn parse_context_turns(value: &str) -> Result<usize> {
    let turns = value.parse::<usize>().context("上下文轮数必须是数字")?;
    if turns > 20 {
        bail!("上下文轮数必须在 0 到 20 之间");
    }
    Ok(turns)
}

fn truncate_history_item(value: &str) -> String {
    let mut output = value
        .chars()
        .take(MAX_HISTORY_ITEM_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_HISTORY_ITEM_CHARS {
        output.push_str("\n…（历史内容已截断）");
    }
    output
}

fn compose_history_prompt(history: &[AiHistoryEntry], current: &str) -> String {
    if history.is_empty() {
        return current.to_owned();
    }
    let mut entries = history
        .iter()
        .map(|entry| {
            let role = if entry.role == "assistant" {
                "助手"
            } else {
                "用户"
            };
            format!("{role}：{}", truncate_history_item(&entry.content))
        })
        .collect::<Vec<_>>();
    let mut total = entries
        .iter()
        .map(|entry| entry.chars().count())
        .sum::<usize>();
    while total > MAX_HISTORY_TOTAL_CHARS && entries.len() > 1 {
        total = total.saturating_sub(entries.remove(0).chars().count());
    }
    format!(
        "以下是同一聊天中最近的对话历史，仅用于理解上下文：\n\n{}\n\n当前请求：\n{}",
        entries.join("\n\n"),
        current
    )
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

fn append_sources(answer: &mut String, sources: &[AiSource]) {
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
            download_ai_photo(client, photo, &path).await?;
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
        bail!("Telegram 未返回可用的图片数据，请重新发送图片后重试");
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

async fn download_ai_photo(client: &Client, photo: &Photo, path: &Path) -> Result<()> {
    let mut variants = photo
        .thumbs()
        .into_iter()
        .filter(|variant| !matches!(variant, PhotoSize::Empty(_) | PhotoSize::Path(_)))
        .collect::<Vec<_>>();
    variants.sort_by_key(|variant| std::cmp::Reverse(variant.size()));

    let variant_count = variants.len();
    let mut failed_attempts = 0usize;
    for variant in variants {
        match client.download_media(&variant, path).await {
            Ok(()) => {
                let byte_len = tokio::fs::metadata(path).await?.len();
                if byte_len > 0 {
                    if failed_attempts > 0 {
                        warn!(
                            photo_id = photo.id(),
                            byte_len,
                            failed_attempts,
                            variant_count,
                            "AI photo download recovered with another Telegram size"
                        );
                    }
                    return Ok(());
                }
                failed_attempts += 1;
            }
            Err(error) => {
                failed_attempts += 1;
                warn!(
                    photo_id = photo.id(),
                    failed_attempts,
                    variant_count,
                    %error,
                    "AI photo size download failed; trying another size"
                );
            }
        }
    }

    client
        .download_media(photo, path)
        .await
        .context("下载回复图片失败")
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

#[async_trait]
trait AiProviderBackend: Send + Sync {
    fn supports_native_search(&self) -> bool;

    async fn generate_search_hedged(
        &self,
        query: &str,
        images: &[AiImage],
        total_limit: Duration,
        hedge_delay: Duration,
    ) -> Result<AiAnswer>;

    async fn generate_chat_with_timeout(
        &self,
        query: &str,
        images: &[AiImage],
        limit: Duration,
    ) -> Result<String>;
}

fn build_provider(
    api_format: AiApiFormat,
    config: &AiConfig,
    base_url: &str,
    primary_model: &str,
    search_fallback_model: &str,
    api_key: String,
) -> Result<Arc<dyn AiProviderBackend>> {
    match api_format {
        AiApiFormat::GeminiInteractions => Ok(Arc::new(GeminiProvider::new_runtime(
            config,
            base_url,
            primary_model,
            search_fallback_model,
            api_key,
        )?)),
        AiApiFormat::OpenaiChatCompletions | AiApiFormat::OpenaiResponses => {
            Ok(Arc::new(OpenAiCompatibleProvider::new_runtime(
                api_format,
                config,
                base_url,
                primary_model,
                search_fallback_model,
                api_key,
            )?))
        }
    }
}

fn native_search_description(api_format: AiApiFormat) -> &'static str {
    match api_format {
        AiApiFormat::GeminiInteractions => "Google Search / Gemini Interactions",
        AiApiFormat::OpenaiResponses => "web_search / OpenAI Responses",
        AiApiFormat::OpenaiChatCompletions => "不支持（Chat Completions 无标准搜索工具）",
    }
}

fn build_ai_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(30))
        .user_agent(concat!("telebot/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to construct AI HTTP client")
}

#[derive(Clone)]
struct OpenAiCompatibleProvider {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    system_prompt: String,
    primary_model: String,
    search_fallback_model: String,
    max_output_tokens: u32,
    api_format: AiApiFormat,
}

impl OpenAiCompatibleProvider {
    fn new_runtime(
        api_format: AiApiFormat,
        config: &AiConfig,
        base_url: &str,
        primary_model: &str,
        search_fallback_model: &str,
        api_key: String,
    ) -> Result<Self> {
        if !matches!(
            api_format,
            AiApiFormat::OpenaiChatCompletions | AiApiFormat::OpenaiResponses
        ) {
            bail!("OpenAI-compatible provider received an unsupported API format");
        }
        validate_model("ai.model", primary_model)?;
        validate_model("ai.search_fallback_model", search_fallback_model)?;
        validate_api_key(&api_key)?;
        let base_url = normalize_base_url(base_url)?;
        let suffix = match api_format {
            AiApiFormat::OpenaiChatCompletions => "chat/completions",
            AiApiFormat::OpenaiResponses => "responses",
            AiApiFormat::GeminiInteractions => unreachable!(),
        };
        let endpoint = append_api_endpoint(&base_url, suffix);
        Ok(Self {
            http: build_ai_http_client()?,
            endpoint,
            api_key,
            system_prompt: config.system_prompt.clone(),
            primary_model: primary_model.to_owned(),
            search_fallback_model: search_fallback_model.to_owned(),
            max_output_tokens: config.max_output_tokens,
            api_format,
        })
    }

    async fn generate_search(
        &self,
        query: &str,
        images: &[AiImage],
        total_limit: Duration,
        hedge_delay: Duration,
    ) -> Result<AiAnswer> {
        if self.api_format != AiApiFormat::OpenaiResponses {
            bail!(
                "{} does not define a standard native web-search tool",
                self.api_format.as_str()
            );
        }
        let started = Instant::now();
        if self.primary_model == self.search_fallback_model {
            return self
                .request(&self.primary_model, query, true, images, total_limit)
                .await
                .map_err(anyhow::Error::from);
        }

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
                    "OpenAI-compatible native search is slow; starting fallback model"
                );
                None
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(500) {
            bail!(
                "OpenAI-compatible native search deadline exceeded after {:.1}s",
                started.elapsed().as_secs_f32()
            );
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

    async fn generate_chat(
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
                    let delay = error.retry_delay();
                    let retry = error.transient
                        && attempt == 0
                        && delay + Duration::from_millis(500) < remaining;
                    last_error = Some(error);
                    if !retry {
                        break;
                    }
                    sleep(delay).await;
                }
            }
        }
        let error = last_error.unwrap_or_else(|| {
            ProviderError::transient("OpenAI-compatible request deadline exceeded")
        });
        Err(anyhow!(
            "OpenAI-compatible request failed after {:.1}s: {error}",
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
    ) -> std::result::Result<AiAnswer, ProviderError> {
        timeout(limit, self.request_inner(model, query, use_search, images))
            .await
            .map_err(|_| ProviderError::transient("OpenAI-compatible request timed out"))?
    }

    async fn request_inner(
        &self,
        model: &str,
        query: &str,
        use_search: bool,
        images: &[AiImage],
    ) -> std::result::Result<AiAnswer, ProviderError> {
        let body = match self.api_format {
            AiApiFormat::OpenaiChatCompletions => build_chat_completions_body(
                model,
                &self.system_prompt,
                query,
                images,
                self.max_output_tokens,
            ),
            AiApiFormat::OpenaiResponses => build_responses_body(
                model,
                &self.system_prompt,
                query,
                images,
                self.max_output_tokens,
                use_search,
            ),
            AiApiFormat::GeminiInteractions => unreachable!(),
        };
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(ProviderError::from_reqwest)?;
        let status = response.status();
        let retry_after = retry_after_delay(response.headers());
        if response
            .content_length()
            .is_some_and(|size| size > MAX_AI_RESPONSE_BYTES as u64)
        {
            return Err(ProviderError::permanent(
                "OpenAI-compatible response was too large",
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(ProviderError::from_reqwest)?;
        if bytes.len() > MAX_AI_RESPONSE_BYTES {
            return Err(ProviderError::permanent(
                "OpenAI-compatible response was too large",
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            ProviderError::permanent(format!("invalid OpenAI-compatible JSON response: {error}"))
        })?;
        if !status.is_success() {
            let message = value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("provider returned an unknown error");
            return Err(ProviderError::http(
                status,
                message,
                retry_after,
                &self.api_key,
            ));
        }
        match self.api_format {
            AiApiFormat::OpenaiChatCompletions => parse_chat_completions_response(&value, model),
            AiApiFormat::OpenaiResponses => parse_responses_response(&value, model, use_search),
            AiApiFormat::GeminiInteractions => unreachable!(),
        }
    }
}

#[async_trait]
impl AiProviderBackend for OpenAiCompatibleProvider {
    fn supports_native_search(&self) -> bool {
        self.api_format == AiApiFormat::OpenaiResponses
    }

    async fn generate_search_hedged(
        &self,
        query: &str,
        images: &[AiImage],
        total_limit: Duration,
        hedge_delay: Duration,
    ) -> Result<AiAnswer> {
        self.generate_search(query, images, total_limit, hedge_delay)
            .await
    }

    async fn generate_chat_with_timeout(
        &self,
        query: &str,
        images: &[AiImage],
        limit: Duration,
    ) -> Result<String> {
        self.generate_chat(query, images, limit).await
    }
}

fn append_api_endpoint(base_url: &str, suffix: &str) -> String {
    if base_url.ends_with(suffix) {
        base_url.to_owned()
    } else {
        format!("{base_url}/{suffix}")
    }
}

fn build_chat_completions_body(
    model: &str,
    system_prompt: &str,
    query: &str,
    images: &[AiImage],
    max_output_tokens: u32,
) -> Value {
    let user_content = if images.is_empty() {
        Value::String(query.to_owned())
    } else {
        let mut content = Vec::with_capacity(images.len() + 1);
        content.push(json!({"type": "text", "text": query}));
        for image in images {
            content.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", image.mime_type, image.base64_data)
                }
            }));
        }
        Value::Array(content)
    };
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_content}
        ],
        "max_tokens": max_output_tokens,
        "stream": false
    })
}

fn build_responses_body(
    model: &str,
    system_prompt: &str,
    query: &str,
    images: &[AiImage],
    max_output_tokens: u32,
    use_search: bool,
) -> Value {
    let mut content = Vec::with_capacity(images.len() + 1);
    content.push(json!({"type": "input_text", "text": query}));
    for image in images {
        content.push(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.base64_data)
        }));
    }
    let instructions = if use_search {
        format!(
            "{system_prompt}\n\n本次请求必须先使用 web_search 获取相关资料，再基于搜索结果回答。"
        )
    } else {
        system_prompt.to_owned()
    };
    let mut body = json!({
        "model": model,
        "instructions": instructions,
        "input": [{"role": "user", "content": content}],
        "max_output_tokens": max_output_tokens,
        "store": false,
        "stream": false
    });
    if use_search {
        body["tools"] = json!([{"type": "web_search"}]);
    }
    body
}

fn parse_chat_completions_response(
    value: &Value,
    requested_model: &str,
) -> std::result::Result<AiAnswer, ProviderError> {
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| ProviderError::permanent("chat completion did not contain a message"))?;
    let mut text = String::new();
    let mut sources = Vec::new();
    let mut seen_sources = HashSet::new();
    match message.get("content") {
        Some(Value::String(content)) => text.push_str(content),
        Some(Value::Array(content)) => {
            for block in content {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                } else if let Some(value) = block.pointer("/text/value").and_then(Value::as_str) {
                    text.push_str(value);
                }
                collect_url_citations(block.get("annotations"), &mut sources, &mut seen_sources);
            }
        }
        _ => {}
    }
    collect_url_citations(message.get("annotations"), &mut sources, &mut seen_sources);
    if text.trim().is_empty() {
        return Err(ProviderError::transient(
            "chat completion returned an empty message",
        ));
    }
    Ok(AiAnswer {
        text,
        sources,
        model: response_model(value, requested_model),
        search_calls: 0,
    })
}

fn parse_responses_response(
    value: &Value,
    requested_model: &str,
    use_search: bool,
) -> std::result::Result<AiAnswer, ProviderError> {
    let mut text = String::new();
    let mut sources = Vec::new();
    let mut seen_sources = HashSet::new();
    let mut search_calls = 0usize;
    for output in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match output.get("type").and_then(Value::as_str) {
            Some("web_search_call") => search_calls += 1,
            Some("message") => {
                for block in output
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if block.get("type").and_then(Value::as_str) == Some("output_text") {
                        if let Some(value) = block.get("text").and_then(Value::as_str) {
                            text.push_str(value);
                        }
                        collect_url_citations(
                            block.get("annotations"),
                            &mut sources,
                            &mut seen_sources,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    if text.trim().is_empty()
        && let Some(output_text) = value.get("output_text").and_then(Value::as_str)
    {
        text.push_str(output_text);
    }
    if text.trim().is_empty() {
        return Err(ProviderError::transient(
            "Responses API returned no output text",
        ));
    }
    if use_search && search_calls == 0 {
        return Err(ProviderError::transient(
            "Responses API did not call the standard web_search tool",
        ));
    }
    Ok(AiAnswer {
        text,
        sources,
        model: response_model(value, requested_model),
        search_calls,
    })
}

fn collect_url_citations(
    annotations: Option<&Value>,
    sources: &mut Vec<AiSource>,
    seen_sources: &mut HashSet<String>,
) {
    for annotation in annotations.and_then(Value::as_array).into_iter().flatten() {
        if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
            continue;
        }
        let citation = annotation.get("url_citation").unwrap_or(annotation);
        let Some(url) = citation.get("url").and_then(Value::as_str) else {
            continue;
        };
        if !url.starts_with("https://") || !seen_sources.insert(url.to_owned()) {
            continue;
        }
        let title = citation
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("参考资料");
        sources.push(AiSource {
            title: title.to_owned(),
            url: url.to_owned(),
        });
    }
}

fn response_model(value: &Value, requested_model: &str) -> String {
    value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(requested_model)
        .to_owned()
}

#[derive(Clone)]
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
    fn new_runtime(
        config: &AiConfig,
        base_url: &str,
        primary_model: &str,
        search_fallback_model: &str,
        api_key: String,
    ) -> Result<Self> {
        validate_model("ai.model", primary_model)?;
        validate_model("ai.search_fallback_model", search_fallback_model)?;
        validate_api_key(&api_key)?;
        let base_url = normalize_base_url(base_url)?;
        let endpoint = if base_url.ends_with("/v1beta") {
            format!("{base_url}/interactions")
        } else {
            format!("{base_url}/v1beta/interactions")
        };
        Ok(Self {
            http: build_ai_http_client()?,
            endpoint,
            api_key,
            system_prompt: config.system_prompt.clone(),
            primary_model: primary_model.to_owned(),
            search_fallback_model: search_fallback_model.to_owned(),
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
    ) -> Result<AiAnswer> {
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
                    let delay = error.retry_delay();
                    let retry = error.transient
                        && attempt == 0
                        && delay + Duration::from_millis(500) < remaining;
                    last_error = Some(error);
                    if !retry {
                        break;
                    }
                    sleep(delay).await;
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
    ) -> std::result::Result<AiAnswer, ProviderError> {
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
    ) -> std::result::Result<AiAnswer, ProviderError> {
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
        let retry_after = retry_after_delay(response.headers());
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
            return Err(ProviderError::http(
                status,
                message,
                retry_after,
                &self.api_key,
            ));
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
                            sources.push(AiSource {
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

        Ok(AiAnswer {
            text,
            sources,
            model: model.to_owned(),
            search_calls,
        })
    }
}

#[async_trait]
impl AiProviderBackend for GeminiProvider {
    fn supports_native_search(&self) -> bool {
        true
    }

    async fn generate_search_hedged(
        &self,
        query: &str,
        images: &[AiImage],
        total_limit: Duration,
        hedge_delay: Duration,
    ) -> Result<AiAnswer> {
        GeminiProvider::generate_search_hedged(self, query, images, total_limit, hedge_delay).await
    }

    async fn generate_chat_with_timeout(
        &self,
        query: &str,
        images: &[AiImage],
        limit: Duration,
    ) -> Result<String> {
        GeminiProvider::generate_chat_with_timeout(self, query, images, limit).await
    }
}

fn native_search_models_failed(
    primary: &ProviderError,
    fallback: &ProviderError,
    elapsed: Duration,
) -> anyhow::Error {
    anyhow!(
        "native search failed after {:.1}s; primary: {}; fallback: {}",
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
    retry_after: Option<Duration>,
}

impl ProviderError {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
            retry_after: None,
        }
    }
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
            retry_after: None,
        }
    }
    fn http(
        status: StatusCode,
        message: &str,
        retry_after: Option<Duration>,
        api_key: &str,
    ) -> Self {
        Self {
            message: format!(
                "provider HTTP {}: {}",
                status.as_u16(),
                sanitize_provider_error(message, api_key)
            ),
            transient: is_transient_status(status),
            retry_after,
        }
    }
    fn from_reqwest(error: reqwest::Error) -> Self {
        Self {
            transient: error.is_timeout() || error.is_connect() || error.is_request(),
            message: error.to_string(),
            retry_after: None,
        }
    }
    fn retry_delay(&self) -> Duration {
        self.retry_after
            .unwrap_or_else(|| Duration::from_millis(150 + rand::random_range(0..150)))
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
        || status == StatusCode::CONFLICT
        || status.is_server_error()
}

fn retry_after_delay(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn sanitize_provider_error(value: &str, api_key: &str) -> String {
    let sanitized = if api_key.is_empty() {
        value.to_owned()
    } else {
        value.replace(api_key, "[redacted]")
    };
    let sanitized = sanitized.replace(['\r', '\n'], " ");
    take_chars(&sanitized, 500)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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
        let sources = vec![AiSource {
            title: "Windows local account [guide]".to_owned(),
            url: "https://example.com/guide".to_owned(),
        }];
        let mut answer = "回答".to_owned();
        append_sources(&mut answer, &sources);
        assert!(answer.contains("### 🔗 来源"));
        assert!(answer.contains("[Windows local account guide]"));
    }

    #[test]
    fn runtime_base_url_is_restricted_and_normalized() {
        assert_eq!(
            normalize_base_url("https://example.com/gemini/").unwrap(),
            "https://example.com/gemini"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(normalize_base_url("http://example.com").is_err());
        assert!(normalize_base_url("https://user@example.com").is_err());
        assert!(normalize_base_url("https://example.com?token=secret").is_err());
    }

    #[test]
    fn openai_endpoints_are_appended_once() {
        assert_eq!(
            append_api_endpoint("https://api.example.com/v1", "chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            append_api_endpoint(
                "https://api.example.com/v1/chat/completions",
                "chat/completions"
            ),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn chat_completions_body_uses_only_standard_fields() {
        let image = AiImage {
            mime_type: "image/png".to_owned(),
            base64_data: "aW1hZ2U=".to_owned(),
            byte_len: 5,
        };
        let body = build_chat_completions_body("test-model", "system", "question", &[image], 512);
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_tokens"], 512);
        assert_eq!(body["stream"], false);
        assert_eq!(
            body.pointer("/messages/1/content/1/type"),
            Some(&json!("image_url"))
        );
        assert_eq!(
            body.pointer("/messages/1/content/1/image_url/url"),
            Some(&json!("data:image/png;base64,aW1hZ2U="))
        );
        assert!(body.get("plugins").is_none());
        assert!(body.get("provider").is_none());
    }

    #[test]
    fn responses_body_adds_only_the_standard_search_tool() {
        let body = build_responses_body("test-model", "system", "question", &[], 512, true);
        assert_eq!(body.pointer("/tools/0/type"), Some(&json!("web_search")));
        assert_eq!(
            body.pointer("/input/0/content/0/type"),
            Some(&json!("input_text"))
        );
        assert_eq!(body["store"], false);
        assert!(body.get("plugins").is_none());
    }

    #[test]
    fn chat_completions_parser_accepts_standard_citations() {
        let value = json!({
            "model": "returned-model",
            "choices": [{
                "message": {
                    "content": "answer",
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {
                            "title": "Example",
                            "url": "https://example.com/source"
                        }
                    }]
                }
            }]
        });
        let answer = parse_chat_completions_response(&value, "requested-model").unwrap();
        assert_eq!(answer.text, "answer");
        assert_eq!(answer.model, "returned-model");
        assert_eq!(answer.sources.len(), 1);
        assert_eq!(answer.sources[0].url, "https://example.com/source");
    }

    #[test]
    fn responses_parser_requires_a_real_search_call() {
        let value = json!({
            "model": "test-model",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "answer",
                    "annotations": [{
                        "type": "url_citation",
                        "title": "Example",
                        "url": "https://example.com/source"
                    }]
                }]
            }]
        });
        let error = parse_responses_response(&value, "test-model", true).unwrap_err();
        assert!(error.transient);
        assert!(error.message.contains("did not call"));

        let mut searched = value;
        searched["output"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"type": "web_search_call", "status": "completed"}));
        let answer = parse_responses_response(&searched, "test-model", true).unwrap();
        assert_eq!(answer.search_calls, 1);
        assert_eq!(answer.sources.len(), 1);
    }

    #[test]
    fn malformed_openai_responses_fail_without_best_effort_text() {
        assert!(parse_chat_completions_response(&json!({"choices": []}), "model").is_err());
        assert!(parse_responses_response(&json!({"output": []}), "model", false).is_err());
    }

    #[test]
    fn provider_errors_are_bounded_and_single_line() {
        let value = format!("secret-looking upstream text\n{}", "x".repeat(800));
        let error = ProviderError::http(StatusCode::BAD_GATEWAY, &value, None, "secret-looking");
        assert!(!error.message.contains('\n'));
        assert!(!error.message.contains("secret-looking"));
        assert!(error.message.chars().count() < 550);
        assert!(error.transient);
    }

    #[tokio::test]
    async fn openai_transport_accepts_json_with_incorrect_content_type() {
        let response = json!({
            "model": "test-model",
            "choices": [{"message": {"content": "transport ok"}}]
        })
        .to_string();
        let (base_url, server) = spawn_single_response_server(response);
        let parsed: crate::config::Config =
            toml::from_str(include_str!("../../config.example.toml")).unwrap();
        let provider = OpenAiCompatibleProvider::new_runtime(
            AiApiFormat::OpenaiChatCompletions,
            &parsed.ai,
            &format!("{base_url}/v1"),
            "test-model",
            "test-model",
            "test-key".to_owned(),
        )
        .unwrap();

        let answer = provider
            .generate_chat("hello", &[], Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(answer, "transport ok");

        let request = server.join().unwrap();
        let request_text = String::from_utf8(request).unwrap();
        let request_lower = request_text.to_ascii_lowercase();
        assert!(request_text.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(request_lower.contains("authorization: bearer test-key\r\n"));
        assert!(request_text.contains("\"model\":\"test-model\""));
        assert!(!request_text.contains("\"plugins\""));
    }

    fn spawn_single_response_server(
        response_body: String,
    ) -> (String, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let mut expected_length = None;
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if expected_length.is_none()
                    && let Some(header_end) = find_bytes(&request, b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected_length = Some(header_end + 4 + content_length);
                }
                if expected_length.is_some_and(|length| request.len() >= length) {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
            request
        });
        (format!("http://{address}"), handle)
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn rolling_history_is_added_before_current_prompt() {
        let history = vec![
            AiHistoryEntry {
                role: "user".to_owned(),
                content: "第一问".to_owned(),
            },
            AiHistoryEntry {
                role: "assistant".to_owned(),
                content: "第一答".to_owned(),
            },
        ];
        let prompt = compose_history_prompt(&history, "第二问");
        assert!(prompt.contains("用户：第一问"));
        assert!(prompt.contains("助手：第一答"));
        assert!(prompt.ends_with("当前请求：\n第二问"));
    }

    #[test]
    fn image_requests_use_the_dedicated_search_timeout() {
        let parsed: crate::config::Config =
            toml::from_str(include_str!("../../config.example.toml"))
                .expect("example config should deserialize");
        let mut config = parsed.ai;
        assert_eq!(effective_search_timeout_seconds(&config, &[]), 20);

        config.image_search_timeout_seconds = 30;
        let image = AiImage {
            mime_type: "image/jpeg".to_owned(),
            base64_data: String::new(),
            byte_len: 0,
        };
        assert_eq!(effective_search_timeout_seconds(&config, &[image]), 30);
    }

    #[tokio::test]
    async fn runtime_tuning_and_progress_messages_load_from_sqlite() {
        let parsed: crate::config::Config =
            toml::from_str(include_str!("../../config.example.toml")).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("runtime.db"))
            .await
            .unwrap();
        store
            .set_setting(AI_DEFAULT_SEARCH_SETTING, "false")
            .await
            .unwrap();
        store
            .set_setting(AI_THINKING_SETTING, "minimal")
            .await
            .unwrap();
        store
            .set_setting(AI_SEARCHING_MESSAGE_SETTING, "🔍 正在查找资料…")
            .await
            .unwrap();
        let defaults = AiDefaults {
            config: parsed.ai,
            messages: parsed.messages,
            env_api_key: "test-key".to_owned(),
        };
        let runtime = runtime_from_store(&defaults, &store).await.unwrap();
        assert!(!runtime.config.default_search);
        assert_eq!(runtime.config.thinking_level, "minimal");
        assert_eq!(runtime.messages.ai_searching, "🔍 正在查找资料…");
        assert_eq!(progress_from_runtime(&runtime).thinking, "💭 正在思考…");
    }
}
