use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub storage: StorageConfig,
    pub ai: AiConfig,
    #[serde(default)]
    pub messages: MessagesConfig,
    pub quote: QuoteConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    pub api_id: i32,
    pub api_hash_env: String,
    pub session_path: PathBuf,
    #[serde(default = "default_prefixes")]
    pub command_prefixes: Vec<String>,
    #[serde(default = "default_true")]
    pub catch_up: bool,
    #[serde(default = "default_parallel_commands")]
    pub max_parallel_commands: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    #[serde(default = "default_ai_api_format")]
    pub api_format: AiApiFormat,
    pub api_key_env: String,
    #[serde(default = "default_gemini_base_url")]
    pub base_url: String,
    pub model: String,
    #[serde(default = "default_search_fallback_model")]
    pub search_fallback_model: String,
    #[serde(default = "default_thinking_level")]
    pub thinking_level: String,
    #[serde(default = "default_true")]
    pub default_search: bool,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    #[serde(default = "default_history_turns")]
    pub history_turns: usize,
    #[serde(default = "default_true")]
    pub collapse_long_messages: bool,
    #[serde(
        default = "default_collapse_threshold",
        rename = "collapse_threshold_chars"
    )]
    pub _collapse_threshold_chars: usize,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_ai_concurrency")]
    pub max_concurrent: usize,
    #[serde(default = "default_search_timeout")]
    pub search_timeout_seconds: u64,
    #[serde(default = "default_image_search_timeout")]
    pub image_search_timeout_seconds: u64,
    #[serde(default = "default_search_hedge")]
    pub search_hedge_seconds: u64,
    #[serde(default = "default_fallback_timeout")]
    pub fallback_timeout_seconds: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiApiFormat {
    GeminiInteractions,
    OpenaiChatCompletions,
    OpenaiResponses,
}

impl AiApiFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GeminiInteractions => "gemini_interactions",
            Self::OpenaiChatCompletions => "openai_chat_completions",
            Self::OpenaiResponses => "openai_responses",
        }
    }

    pub const fn supports_native_search(self) -> bool {
        matches!(self, Self::GeminiInteractions | Self::OpenaiResponses)
    }
}

impl FromStr for AiApiFormat {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gemini_interactions" => Ok(Self::GeminiInteractions),
            "openai_chat_completions" => Ok(Self::OpenaiChatCompletions),
            "openai_responses" => Ok(Self::OpenaiResponses),
            _ => bail!(
                "AI API format must be gemini_interactions, openai_chat_completions, or openai_responses"
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MessagesConfig {
    pub ai_searching: String,
    pub ai_thinking: String,
}

impl Default for MessagesConfig {
    fn default() -> Self {
        Self {
            ai_searching: "🔎 正在搜索…".to_owned(),
            ai_thinking: "💭 正在思考…".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_quote_api")]
    pub api_url: String,
    #[serde(default = "default_quote_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_quote_messages")]
    pub max_messages: usize,
    #[serde(default = "default_quote_background")]
    pub background_color: String,
    #[serde(default)]
    pub sticker_set_short_name: String,
    #[serde(default)]
    pub history_enabled: bool,
    #[serde(default = "default_quote_history_limit")]
    pub history_limit: usize,
    #[serde(default = "default_quote_history_max_bytes")]
    pub history_max_bytes: usize,
}

#[derive(Clone)]
pub struct Secrets {
    pub telegram_api_hash: String,
    pub ai_api_key: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let config: Self =
            toml::from_str(&raw).with_context(|| format!("invalid config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.telegram.api_id <= 0 {
            bail!("telegram.api_id must be positive");
        }
        if self.telegram.api_hash_env.trim().is_empty() {
            bail!("telegram.api_hash_env must not be empty");
        }
        if self.telegram.command_prefixes.is_empty()
            || self
                .telegram
                .command_prefixes
                .iter()
                .any(|prefix| prefix.is_empty())
        {
            bail!("telegram.command_prefixes must contain non-empty strings");
        }
        if self.telegram.max_parallel_commands == 0 {
            bail!("telegram.max_parallel_commands must be at least 1");
        }
        if self.ai.enabled {
            if self.ai.provider.trim().is_empty()
                || self.ai.provider.len() > 32
                || !self
                    .ai
                    .provider
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            {
                bail!(
                    "ai.provider must contain only letters, numbers, dots, hyphens, or underscores and be at most 32 characters"
                );
            }
            if self.ai.api_key_env.trim().is_empty()
                || self.ai.base_url.trim().is_empty()
                || self.ai.model.trim().is_empty()
            {
                bail!("AI key, base URL, and model settings must not be empty");
            }
            if self.ai.api_format.supports_native_search()
                && self.ai.search_fallback_model.trim().is_empty()
            {
                bail!("ai.search_fallback_model is required for native-search API formats");
            }
            if !matches!(
                self.ai.thinking_level.as_str(),
                "minimal" | "low" | "medium" | "high"
            ) {
                bail!("ai.thinking_level must be minimal, low, medium, or high");
            }
            if self.ai.max_concurrent == 0 {
                bail!("ai.max_concurrent must be at least 1");
            }
            if self.ai.history_turns > 20 {
                bail!("ai.history_turns must be between 0 and 20");
            }
            if !(1..=65_536).contains(&self.ai.max_output_tokens) {
                bail!("ai.max_output_tokens must be between 1 and 65536");
            }
            if !(3..=120).contains(&self.ai.search_timeout_seconds)
                || !(3..=120).contains(&self.ai.image_search_timeout_seconds)
                || !(3..=120).contains(&self.ai.fallback_timeout_seconds)
            {
                bail!("AI timeouts must be between 3 and 120 seconds");
            }
            if self.ai.image_search_timeout_seconds < self.ai.search_timeout_seconds {
                bail!("ai.image_search_timeout_seconds must be at least search timeout");
            }
            if !(3..self.ai.search_timeout_seconds).contains(&self.ai.search_hedge_seconds) {
                bail!("ai.search_hedge_seconds must be at least 3 and below search timeout");
            }
        }
        validate_progress_message("messages.ai_searching", &self.messages.ai_searching)?;
        validate_progress_message("messages.ai_thinking", &self.messages.ai_thinking)?;
        if self.quote.enabled {
            if !(1..=10).contains(&self.quote.max_messages) {
                bail!("quote.max_messages must be between 1 and 10");
            }
            if !(3..=120).contains(&self.quote.timeout_seconds) {
                bail!("quote.timeout_seconds must be between 3 and 120 seconds");
            }
            if !(1..=500).contains(&self.quote.history_limit) {
                bail!("quote.history_limit must be between 1 and 500");
            }
            if !(1024 * 1024..=1024 * 1024 * 1024).contains(&self.quote.history_max_bytes) {
                bail!("quote.history_max_bytes must be between 1 MiB and 1 GiB");
            }
            let quote_url = self.quote.api_url.as_str();
            let secure = quote_url.starts_with("https://");
            let loopback = quote_url.starts_with("http://127.0.0.1:")
                || quote_url.starts_with("http://localhost:")
                || quote_url.starts_with("http://[::1]:");
            if !secure && !loopback {
                bail!("quote.api_url must use https or a loopback-only http endpoint");
            }
        }
        Ok(())
    }

    pub fn load_secrets(&self) -> Result<Secrets> {
        let telegram_api_hash = required_env(&self.telegram.api_hash_env)?;
        let ai_api_key = if self.ai.enabled {
            Some(required_env(&self.ai.api_key_env)?)
        } else {
            None
        };
        Ok(Secrets {
            telegram_api_hash,
            ai_api_key,
        })
    }
}

fn required_env(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("required environment variable {name} is missing"))?;
    if value.trim().is_empty() {
        bail!("required environment variable {name} is empty");
    }
    Ok(value)
}

fn default_true() -> bool {
    true
}
fn default_prefixes() -> Vec<String> {
    [".", "。", ",", "，", "$", "!", "！"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}
fn default_parallel_commands() -> usize {
    8
}
fn default_ai_provider() -> String {
    "gemini".to_owned()
}
fn default_ai_api_format() -> AiApiFormat {
    AiApiFormat::GeminiInteractions
}
fn default_gemini_base_url() -> String {
    "https://generativelanguage.googleapis.com".to_owned()
}
fn default_search_fallback_model() -> String {
    "gemini-3.1-flash-lite".to_owned()
}
fn default_thinking_level() -> String {
    "minimal".to_owned()
}
fn default_system_prompt() -> String {
    "请使用中文准确、简洁地回答。使用联网搜索时优先采用可靠来源，并明确区分事实与推断。".to_owned()
}
fn default_history_turns() -> usize {
    0
}
fn default_collapse_threshold() -> usize {
    400
}
fn default_max_output_tokens() -> u32 {
    4096
}
fn default_ai_concurrency() -> usize {
    3
}
fn default_search_timeout() -> u64 {
    20
}

fn validate_progress_message(name: &str, value: &str) -> Result<()> {
    let length = value.chars().count();
    if value.trim().is_empty() || length > 128 || value.contains('\r') || value.contains('\n') {
        bail!("{name} must be a non-empty single line of at most 128 characters");
    }
    Ok(())
}
fn default_image_search_timeout() -> u64 {
    30
}
fn default_search_hedge() -> u64 {
    10
}
fn default_fallback_timeout() -> u64 {
    10
}
fn default_quote_api() -> String {
    "http://127.0.0.1:3210/generate".to_owned()
}
fn default_quote_timeout() -> u64 {
    20
}
fn default_max_quote_messages() -> usize {
    5
}
fn default_quote_background() -> String {
    "#1b1429".to_owned()
}
fn default_quote_history_limit() -> usize {
    100
}
fn default_quote_history_max_bytes() -> usize {
    100 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_include_requested_prefixes() {
        let prefixes = default_prefixes();
        for expected in [".", "。", ",", "，"] {
            assert!(prefixes.iter().any(|prefix| prefix == expected));
        }
    }

    #[test]
    fn image_search_default_is_longer_than_text_search() {
        assert!(default_image_search_timeout() > default_search_timeout());
    }

    #[test]
    fn api_formats_use_explicit_stable_names() {
        assert_eq!(
            "openai_chat_completions".parse::<AiApiFormat>().unwrap(),
            AiApiFormat::OpenaiChatCompletions
        );
        assert_eq!(
            "openai_responses".parse::<AiApiFormat>().unwrap(),
            AiApiFormat::OpenaiResponses
        );
        assert!("openrouter".parse::<AiApiFormat>().is_err());
    }

    #[test]
    fn openai_compatible_config_does_not_require_a_named_gateway() {
        let raw = include_str!("../config.example.toml")
            .replace("api_id = 0", "api_id = 1")
            .replace("provider = \"gemini\"", "provider = \"generic-oai\"")
            .replace(
                "api_format = \"gemini_interactions\"",
                "api_format = \"openai_chat_completions\"",
            )
            .replace(
                "base_url = \"https://generativelanguage.googleapis.com\"",
                "base_url = \"https://api.example.com/v1\"",
            )
            .replace("default_search = true", "default_search = false");
        let parsed: Config = toml::from_str(&raw).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.ai.provider, "generic-oai");
        assert_eq!(parsed.ai.api_format, AiApiFormat::OpenaiChatCompletions);
    }

    #[test]
    fn message_defaults_are_available_without_a_toml_section() {
        let raw = include_str!("../config.example.toml").replace(
            "\n[messages]\nai_searching = \"🔎 正在搜索…\"\nai_thinking = \"💭 正在思考…\"\n",
            "\n",
        );
        let parsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.messages.ai_searching, "🔎 正在搜索…");
        assert_eq!(parsed.messages.ai_thinking, "💭 正在思考…");
    }
}
