mod command;
mod config;
mod plugin;
mod plugins;
mod session_import;
mod session_login;
mod store;
mod telegram;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use grammers_client::Client;
use grammers_client::client::UpdatesConfiguration;
use grammers_client::media::Media;
use grammers_client::message::InputMessage;
use grammers_client::update::Update;
use grammers_mtsender::SenderPool;
use grammers_session::Session;
use grammers_session::storages::SqliteSession;
use grammers_session::types::PeerId;
use grammers_session::updates::UpdatesLike;
use grammers_tl_types as tl;
use tokio::sync::{RwLock, Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::command::{Command, parse};
use crate::config::Config;
use crate::plugin::{CommandContext, Plugin, Router};
use crate::plugins::ai::{AiPlugin, AiProgressConfig};
use crate::plugins::quote::QuotePlugin;
use crate::store::Store;
use crate::telegram::replace_with_chunks;

const MAX_TELEGRAM_SMOKE_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
struct RawCommandCandidate {
    peer_id: PeerId,
    message_id: i32,
    date: i32,
    command: Command,
}

#[derive(Parser)]
#[command(name = "telebot", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authorize a Telegram user session using a phone number and login code.
    Login {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    Serve {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    Validate {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    CheckSession {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    CheckAi {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    CheckQuote {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    CheckTelegramFormat {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    CheckTelegramPlugins {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
    },
    CheckTelegramImage {
        #[arg(long, default_value = "/etc/telebot/config.toml")]
        config: PathBuf,
        #[arg(long)]
        image: PathBuf,
    },
    ImportGramjsSession {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("telebot=info,warn")),
        )
        .with_target(false)
        .compact()
        .init();

    match Cli::parse().command {
        Commands::Login { config } => session_login::login(&config).await,
        Commands::Serve { config } => serve(&config).await,
        Commands::Validate { config } => {
            let config = Config::load(&config)?;
            config.load_secrets()?;
            println!("Configuration is valid");
            Ok(())
        }
        Commands::CheckSession { config } => check_session(&config).await,
        Commands::CheckAi { config } => {
            let config = Config::load(&config)?;
            let secrets = config.load_secrets()?;
            plugins::ai::check_provider(
                &config.ai,
                secrets.ai_api_key.expect("AI key checked by config"),
            )
            .await
        }
        Commands::CheckQuote { config } => {
            let config = Config::load(&config)?;
            plugins::quote::check_provider(&config.quote).await
        }
        Commands::CheckTelegramFormat { config } => check_telegram_format(&config).await,
        Commands::CheckTelegramPlugins { config } => check_telegram_plugins(&config).await,
        Commands::CheckTelegramImage { config, image } => {
            check_telegram_image(&config, &image).await
        }
        Commands::ImportGramjsSession { from, to } => {
            session_import::import_gramjs_config(&from, &to).await
        }
    }
}

async fn check_telegram_image(config_path: &Path, image_path: &Path) -> Result<()> {
    let config = Config::load(config_path)?;
    let secrets = config.load_secrets()?;
    let _telegram_api_hash = &secrets.telegram_api_hash;
    let metadata = tokio::fs::metadata(image_path)
        .await
        .with_context(|| format!("failed to inspect {}", image_path.display()))?;
    if metadata.len() == 0 || metadata.len() > MAX_TELEGRAM_SMOKE_IMAGE_BYTES {
        bail!("Telegram image smoke input must be between 1 byte and 8 MB");
    }

    let temporary = tempfile::tempdir()?;
    let store = Arc::new(Store::open(&temporary.path().join("check.db")).await?);
    let session = Arc::new(SqliteSession::open(&config.telegram.session_path).await?);
    let SenderPool { runner, handle, .. } =
        SenderPool::new(Arc::clone(&session), config.telegram.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());
    let result = async {
        if !client.is_authorized().await? {
            bail!("Telegram session is not authorized");
        }
        let me = client.get_me().await?;
        let peer = me
            .to_ref()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .context("Telegram self peer is unavailable")?;

        let uploaded = client.upload_file(image_path).await?;
        let image_source = client
            .send_message(peer, InputMessage::new().photo(uploaded))
            .await?;
        let ai_command = client
            .send_message(
                peer,
                InputMessage::new()
                    .text(".telebot_image_smoke")
                    .reply_to(Some(image_source.id())),
            )
            .await?;
        let progress = Arc::new(RwLock::new(AiProgressConfig::new(
            &config.ai,
            &config.messages,
        )));
        let ai = AiPlugin::new(
            config_path.to_path_buf(),
            config.ai.clone(),
            config.messages.clone(),
            secrets.ai_api_key.context("AI key is missing")?,
            Arc::clone(&store),
            Arc::clone(&progress),
        )
        .await?;
        let ai_result = ai
            .handle(CommandContext {
                client: client.clone(),
                message: ai_command.clone(),
                command: Command {
                    prefix: ".".to_owned(),
                    name: "ai".to_owned(),
                    raw_args: "search".to_owned(),
                    args: vec!["search".to_owned()],
                },
            })
            .await;
        let refreshed_ai = client
            .get_messages_by_id(peer, &[ai_command.id()])
            .await?
            .pop()
            .flatten();
        let ai_valid = refreshed_ai.as_ref().is_some_and(|message| {
            message.text().contains("A:") && message.text().ends_with("🍀 Powered by Gemini")
        });
        if let Some(message) = refreshed_ai {
            let _ = message.delete().await;
        } else {
            let _ = ai_command.delete().await;
        }
        let _ = image_source.delete().await;
        ai_result?;
        if !ai_valid {
            bail!("AI image smoke test did not produce a Gemini answer");
        }

        println!("Telegram AI image check passed; temporary items deleted");
        Ok(())
    }
    .await;
    handle.quit();
    let _ = pool_task.await;
    result
}

async fn check_telegram_plugins(config_path: &Path) -> Result<()> {
    let config = Config::load(config_path)?;
    let secrets = config.load_secrets()?;
    let _telegram_api_hash = &secrets.telegram_api_hash;
    let temporary = tempfile::tempdir()?;
    let store = Arc::new(Store::open(&temporary.path().join("check.db")).await?);
    let session = Arc::new(SqliteSession::open(&config.telegram.session_path).await?);
    let SenderPool { runner, handle, .. } =
        SenderPool::new(Arc::clone(&session), config.telegram.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());
    let result = async {
        if !client.is_authorized().await? {
            bail!("Telegram session is not authorized");
        }
        let me = client.get_me().await?;
        let peer = me
            .to_ref()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .context("Telegram self peer is unavailable")?;

        let ai_source = client
            .send_message(peer, "请只回答 7319 加 1 的结果。")
            .await?;
        let ai_command = client
            .send_message(
                peer,
                InputMessage::new()
                    .text(".telebot_smoke")
                    .reply_to(Some(ai_source.id())),
            )
            .await?;
        let progress = Arc::new(RwLock::new(AiProgressConfig::new(
            &config.ai,
            &config.messages,
        )));
        let ai = AiPlugin::new(
            config_path.to_path_buf(),
            config.ai.clone(),
            config.messages.clone(),
            secrets.ai_api_key.context("AI key is missing")?,
            Arc::clone(&store),
            Arc::clone(&progress),
        )
        .await?;
        let ai_result = ai
            .handle(CommandContext {
                client: client.clone(),
                message: ai_command.clone(),
                command: Command {
                    prefix: ".".to_owned(),
                    name: "ai".to_owned(),
                    raw_args: String::new(),
                    args: Vec::new(),
                },
            })
            .await;
        let refreshed_ai = client
            .get_messages_by_id(peer, &[ai_command.id()])
            .await?
            .pop()
            .flatten();
        let ai_valid = refreshed_ai.as_ref().is_some_and(|message| {
            message.text().contains("7320")
                && message.text().ends_with("🍀 Powered by Gemini")
                && !message.text().contains("联网搜索")
                && !message.text().contains("普通回答")
        });
        if let Some(message) = refreshed_ai {
            let _ = message.delete().await;
        } else {
            let _ = ai_command.delete().await;
        }
        let _ = ai_source.delete().await;
        ai_result?;
        if !ai_valid {
            bail!("AI reply smoke test did not use the replied message");
        }

        let help_command = client.send_message(peer, ".telebot_help_smoke").await?;
        let help_result = ai
            .handle(CommandContext {
                client: client.clone(),
                message: help_command.clone(),
                command: Command {
                    prefix: ".".to_owned(),
                    name: "ai".to_owned(),
                    raw_args: "help".to_owned(),
                    args: vec!["help".to_owned()],
                },
            })
            .await;
        let refreshed_help = client
            .get_messages_by_id(peer, &[help_command.id()])
            .await?
            .pop()
            .flatten();
        let help_valid = refreshed_help.as_ref().is_some_and(|message| {
            message.text().contains("TeleBot AI 帮助")
                && message.text().contains("回复消息与图片")
                && message.text().contains("动态配置（仅限收藏夹）")
                && message.text().contains("config reload")
        });
        if let Some(message) = refreshed_help {
            let _ = message.delete().await;
        } else {
            let _ = help_command.delete().await;
        }
        help_result?;
        if !help_valid {
            bail!("AI help smoke test did not contain the detailed guide");
        }

        let config_command = client.send_message(peer, ".telebot_config_smoke").await?;
        let config_result = ai
            .handle(CommandContext {
                client: client.clone(),
                message: config_command.clone(),
                command: Command {
                    prefix: ".".to_owned(),
                    name: "ai".to_owned(),
                    raw_args: "config message searching 🔍 配置化搜索测试".to_owned(),
                    args: vec![
                        "config".to_owned(),
                        "message".to_owned(),
                        "searching".to_owned(),
                        "🔍".to_owned(),
                        "配置化搜索测试".to_owned(),
                    ],
                },
            })
            .await;
        let refreshed_config = client
            .get_messages_by_id(peer, &[config_command.id()])
            .await?
            .pop()
            .flatten();
        let config_valid = refreshed_config
            .as_ref()
            .is_some_and(|message| message.text().contains("进度文案已更新"))
            && progress.read().await.searching == "🔍 配置化搜索测试";
        if let Some(message) = refreshed_config {
            let _ = message.delete().await;
        } else {
            let _ = config_command.delete().await;
        }
        config_result?;
        if !config_valid {
            bail!("AI dynamic message smoke test did not update the live progress configuration");
        }

        let quote_source = client
            .send_message(peer, "telebot 中文语录验收：你好，世界！")
            .await?;
        let quote_command = client
            .send_message(
                peer,
                InputMessage::new()
                    .text(".telebot_smoke")
                    .reply_to(Some(quote_source.id())),
            )
            .await?;
        let quote = QuotePlugin::new(config.quote.clone(), Arc::clone(&store))?;
        let quote_result = quote
            .handle(CommandContext {
                client: client.clone(),
                message: quote_command.clone(),
                command: Command {
                    prefix: ".".to_owned(),
                    name: "q".to_owned(),
                    raw_args: String::new(),
                    args: Vec::new(),
                },
            })
            .await;
        let mut quote_output = None;
        let mut messages = client.iter_messages(peer).limit(12);
        while let Some(message) = messages.next().await? {
            if message.id() > quote_command.id()
                && message.reply_to_message_id() == Some(quote_source.id())
                && matches!(message.media(), Some(Media::Sticker(_)))
            {
                quote_output = Some(message);
                break;
            }
        }
        let quote_valid = quote_output.is_some();
        if let Some(message) = quote_output {
            let _ = message.delete().await;
        }
        let _ = quote_command.delete().await;
        let _ = quote_source.delete().await;
        quote_result?;
        if !quote_valid {
            bail!("quote smoke test did not produce a Telegram sticker reply");
        }

        println!(
            "Telegram plugin checks passed: .ai reply, detailed help, dynamic message config and .q sticker; temporary items deleted"
        );
        Ok(())
    }
    .await;
    handle.quit();
    let _ = pool_task.await;
    result
}

async fn check_telegram_format(config_path: &Path) -> Result<()> {
    let config = Config::load(config_path)?;
    let secrets = config.load_secrets()?;
    let _telegram_api_hash = &secrets.telegram_api_hash;
    let session = Arc::new(SqliteSession::open(&config.telegram.session_path).await?);
    let SenderPool { runner, handle, .. } =
        SenderPool::new(Arc::clone(&session), config.telegram.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());
    let result = async {
        if !client.is_authorized().await? {
            bail!("Telegram session is not authorized");
        }
        let me = client.get_me().await?;
        let peer = me
            .to_ref()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .context("Telegram self peer is unavailable")?;
        let answer =
            "## 富文本检查\n\n- **粗体**、*斜体*、`代码`\n- [链接](https://example.com)\n\n"
                .to_owned()
                + &"这是一段用于确认 Telegram 可展开引用实体的文字。".repeat(20);
        let rich = telegram::ai_rich_response("一次性服务器验收", &answer, "telebot", true);
        let message = client.send_message(peer, rich.input_message()).await?;
        let expandable = message.fmt_entities().is_some_and(|entities| {
            entities.iter().any(|entity| {
                matches!(
                    entity,
                    grammers_tl_types::enums::MessageEntity::Blockquote(value) if value.collapsed
                )
            })
        });
        message.delete().await?;
        if !expandable {
            bail!("Telegram did not preserve the expandable blockquote entity");
        }
        println!("Telegram rich-text check passed; temporary Saved Messages item deleted");
        Ok(())
    }
    .await;
    handle.quit();
    let _ = pool_task.await;
    result
}

async fn check_session(config_path: &Path) -> Result<()> {
    let config = Config::load(config_path)?;
    let secrets = config.load_secrets()?;
    let _telegram_api_hash = &secrets.telegram_api_hash;
    let session = Arc::new(SqliteSession::open(&config.telegram.session_path).await?);
    let SenderPool { runner, handle, .. } =
        SenderPool::new(Arc::clone(&session), config.telegram.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());
    let result = async {
        if !client.is_authorized().await? {
            bail!("Telegram session is not authorized");
        }
        let me = client.get_me().await?;
        let dialogs = warm_peer_cache(&client).await?;
        println!(
            "Telegram session is authorized for user {} (@{}); cached {} dialogs",
            me.id().bare_id().unwrap_or_default(),
            me.username().unwrap_or(""),
            dialogs
        );
        Ok(())
    }
    .await;
    handle.quit();
    let _ = pool_task.await;
    result
}

async fn warm_peer_cache(client: &Client) -> Result<usize> {
    let mut dialogs = client.iter_dialogs();
    let mut count = 0usize;
    while dialogs.next().await?.is_some() {
        count += 1;
    }
    Ok(count)
}

fn raw_command_candidate(
    peer_id: PeerId,
    message_id: i32,
    date: i32,
    text: &str,
    belongs_to_self: bool,
    prefixes: &[String],
) -> Option<RawCommandCandidate> {
    if !belongs_to_self {
        return None;
    }
    Some(RawCommandCandidate {
        peer_id,
        message_id,
        date,
        command: parse(text, prefixes)?,
    })
}

fn raw_command_from_message(
    message: &tl::enums::Message,
    self_id: PeerId,
    prefixes: &[String],
) -> Option<RawCommandCandidate> {
    let tl::enums::Message::Message(message) = message else {
        return None;
    };
    let peer_id = PeerId::from(&message.peer_id);
    let self_sender = message
        .from_id
        .as_ref()
        .is_some_and(|sender| PeerId::from(sender) == self_id);
    raw_command_candidate(
        peer_id,
        message.id,
        message.date,
        &message.message,
        command_belongs_to_self(message.out, self_sender, peer_id == self_id),
        prefixes,
    )
}

fn raw_command_from_update(
    update: &tl::enums::Update,
    self_id: PeerId,
    prefixes: &[String],
) -> Option<RawCommandCandidate> {
    match update {
        tl::enums::Update::NewMessage(update) => {
            raw_command_from_message(&update.message, self_id, prefixes)
        }
        tl::enums::Update::NewChannelMessage(update) => {
            raw_command_from_message(&update.message, self_id, prefixes)
        }
        _ => None,
    }
}

fn raw_command_candidates(
    updates: &UpdatesLike,
    self_id: PeerId,
    prefixes: &[String],
) -> Vec<RawCommandCandidate> {
    let UpdatesLike::Updates(updates) = updates else {
        return Vec::new();
    };
    match updates {
        tl::enums::Updates::UpdateShortMessage(update) => {
            let Some(peer_id) = PeerId::user(update.user_id) else {
                return Vec::new();
            };
            raw_command_candidate(
                peer_id,
                update.id,
                update.date,
                &update.message,
                command_belongs_to_self(update.out, peer_id == self_id, peer_id == self_id),
                prefixes,
            )
            .into_iter()
            .collect()
        }
        tl::enums::Updates::UpdateShortChatMessage(update) => {
            let (Some(peer_id), Some(sender_id)) =
                (PeerId::chat(update.chat_id), PeerId::user(update.from_id))
            else {
                return Vec::new();
            };
            raw_command_candidate(
                peer_id,
                update.id,
                update.date,
                &update.message,
                command_belongs_to_self(update.out, sender_id == self_id, false),
                prefixes,
            )
            .into_iter()
            .collect()
        }
        tl::enums::Updates::UpdateShort(update) => {
            raw_command_from_update(&update.update, self_id, prefixes)
                .into_iter()
                .collect()
        }
        tl::enums::Updates::Combined(updates) => updates
            .updates
            .iter()
            .filter_map(|update| raw_command_from_update(update, self_id, prefixes))
            .collect(),
        tl::enums::Updates::Updates(updates) => updates
            .updates
            .iter()
            .filter_map(|update| raw_command_from_update(update, self_id, prefixes))
            .collect(),
        tl::enums::Updates::TooLong | tl::enums::Updates::UpdateShortSentMessage(_) => Vec::new(),
    }
}

fn command_progress_text(command: &Command, progress: &AiProgressConfig) -> Option<String> {
    let mut parts = command.raw_args.split_whitespace();
    let first = parts.next().unwrap_or("").to_ascii_lowercase();
    let second = parts.next().unwrap_or("").to_ascii_lowercase();
    match command.name.as_str() {
        "ai" if matches!(first.as_str(), "config" | "cfg") && second == "key" => {
            Some("🔐 正在安全更新 AI 配置…".to_owned())
        }
        "ai" if matches!(
            first.as_str(),
            "help" | "?" | "status" | "config" | "cfg" | "context" | "ctx" | "reset" | "clear"
        ) =>
        {
            None
        }
        "ai" if matches!(first.as_str(), "chat" | "c") => Some(progress.thinking.clone()),
        "ai" if matches!(first.as_str(), "search" | "s") => Some(progress.searching.clone()),
        "ai" if progress.default_search => Some(progress.searching.clone()),
        "ai" => Some(progress.thinking.clone()),
        "q" if matches!(first.as_str(), "config" | "help" | "h") => None,
        "q" if matches!(first.as_str(), "history" | "his") => {
            Some("🗂️ 正在读取语录存档…".to_owned())
        }
        "q" if first == "s" => Some("📦 正在保存到贴纸包…".to_owned()),
        "q" => Some("🖼️ 正在生成语录贴纸…".to_owned()),
        _ => None,
    }
}

fn remember_command(commands: &mut HashMap<(PeerId, i32), Instant>, key: (PeerId, i32)) -> bool {
    const DEDUP_TTL: Duration = Duration::from_secs(10 * 60);
    commands.retain(|_, started| started.elapsed() < DEDUP_TTL);
    commands.insert(key, Instant::now()).is_none()
}

async fn execute_fast_command(
    session: Arc<SqliteSession>,
    client: Client,
    plugin: Arc<dyn Plugin>,
    capacity: Arc<Semaphore>,
    candidate: RawCommandCandidate,
    progress: Option<String>,
) {
    let started = Instant::now();
    let peer = match session.peer_ref(candidate.peer_id).await {
        Ok(Some(peer)) => peer,
        Ok(None) => candidate.peer_id.to_ambient_ref(),
        Err(error) => {
            warn!(%error, peer_id = %candidate.peer_id, "fast command peer lookup failed; using ambient reference");
            candidate.peer_id.to_ambient_ref()
        }
    };
    if let Some(progress) = progress {
        match tokio::time::timeout(
            Duration::from_secs(3),
            client.edit_message(
                peer,
                candidate.message_id,
                InputMessage::new().text(progress).link_preview(false),
            ),
        )
        .await
        {
            Ok(Ok(())) => info!(
                message_id = candidate.message_id,
                elapsed_ms = started.elapsed().as_millis(),
                "raw command progress displayed"
            ),
            Ok(Err(error)) => warn!(
                message_id = candidate.message_id,
                %error,
                "raw command progress edit failed"
            ),
            Err(_) => warn!(
                message_id = candidate.message_id,
                "raw command progress edit timed out"
            ),
        }
    }

    let queued_at = Instant::now();
    let _permit = match capacity.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return,
    };
    info!(
        message_id = candidate.message_id,
        queue_ms = queued_at.elapsed().as_millis(),
        "fast command worker acquired"
    );
    let message = match client
        .get_messages_by_id(peer, &[candidate.message_id])
        .await
    {
        Ok(mut messages) => match messages.pop().flatten() {
            Some(message) => message,
            None => {
                error!(
                    message_id = candidate.message_id,
                    "fast command message was not found"
                );
                return;
            }
        },
        Err(error) => {
            error!(message_id = candidate.message_id, %error, "fast command message fetch failed");
            let text = format!("❌ {}", user_facing_error(&error.into()));
            let _ = client
                .edit_message(peer, candidate.message_id, InputMessage::new().text(text))
                .await;
            return;
        }
    };
    let context = CommandContext {
        client: client.clone(),
        message: message.clone(),
        command: candidate.command,
    };
    if let Err(error) = plugin.handle(context).await {
        error!(plugin = plugin.name(), %error, "fast command failed");
        let text = format!("❌ {}", user_facing_error(&error));
        if let Err(edit_error) = replace_with_chunks(&client, &message, &text).await {
            error!(%edit_error, "failed to show fast command error");
        }
    }
}

async fn serve(config_path: &Path) -> Result<()> {
    let config = Config::load(config_path)?;
    let secrets = config.load_secrets()?;
    let _telegram_api_hash = &secrets.telegram_api_hash;
    let store = Arc::new(Store::open(&config.storage.path).await?);
    if let Some(parent) = config.telegram.session_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let session = Arc::new(SqliteSession::open(&config.telegram.session_path).await?);
    let SenderPool {
        runner,
        updates,
        handle,
    } = SenderPool::new(Arc::clone(&session), config.telegram.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());

    if !client.is_authorized().await? {
        handle.quit();
        let _ = pool_task.await;
        bail!("Telegram session is not authorized; import an existing GramJS session first");
    }
    let me = client.get_me().await?;
    let self_id = me.id();
    info!(
        user_id = me.id().bare_id(),
        username = me.username().unwrap_or(""),
        "Telegram session authorized"
    );
    match tokio::time::timeout(std::time::Duration::from_secs(30), warm_peer_cache(&client)).await {
        Ok(Ok(dialogs)) => info!(dialogs, "Telegram peer cache warmed"),
        Ok(Err(error)) => warn!(%error, "Telegram peer cache warmup failed; continuing"),
        Err(_) => warn!("Telegram peer cache warmup exceeded 30 seconds; continuing"),
    }

    let ai_progress = Arc::new(RwLock::new(AiProgressConfig::new(
        &config.ai,
        &config.messages,
    )));
    let mut router = Router::default();
    if config.ai.enabled {
        router.register(Arc::new(
            AiPlugin::new(
                config_path.to_path_buf(),
                config.ai.clone(),
                config.messages.clone(),
                secrets.ai_api_key.expect("AI key checked by config"),
                Arc::clone(&store),
                Arc::clone(&ai_progress),
            )
            .await?,
        ))?;
    }
    if config.quote.enabled {
        router.register(Arc::new(QuotePlugin::new(
            config.quote.clone(),
            Arc::clone(&store),
        )?))?;
    }
    info!(commands = ?router.registered_commands(), "plugins loaded");
    let router = Arc::new(router);
    let command_capacity = Arc::new(Semaphore::new(config.telegram.max_parallel_commands));
    let mut tasks = JoinSet::new();
    let (ordered_updates_tx, ordered_updates) = mpsc::unbounded_channel();
    let (fast_commands_tx, mut fast_commands) = mpsc::unbounded_channel();
    let prefixes = config.telegram.command_prefixes.clone();
    let raw_updates_task = tokio::spawn(async move {
        let mut raw_updates = updates;
        while let Some(raw_update) = raw_updates.recv().await {
            for candidate in raw_command_candidates(&raw_update, self_id, &prefixes) {
                if fast_commands_tx.send(candidate).is_err() {
                    return;
                }
            }
            if ordered_updates_tx.send(raw_update).is_err() {
                return;
            }
        }
    });
    let mut updates = client
        .stream_updates(
            ordered_updates,
            UpdatesConfiguration {
                catch_up: config.telegram.catch_up,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    info!("telebot is ready");
    let mut started_commands = HashMap::new();
    let mut fast_commands_open = true;
    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                warn!(%error, "command task panicked");
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested");
                break;
            }
            candidate = fast_commands.recv(), if fast_commands_open => {
                let Some(candidate) = candidate else {
                    fast_commands_open = false;
                    continue;
                };
                let Some(plugin) = router.plugin_for(&candidate.command.name) else { continue };
                let key = (candidate.peer_id, candidate.message_id);
                if !remember_command(&mut started_commands, key) {
                    continue;
                }
                let received_at_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i128;
                let raw_age_ms = received_at_ms.saturating_sub(candidate.date as i128 * 1_000);
                info!(
                    message_id = candidate.message_id,
                    command = candidate.command.name,
                    raw_age_ms,
                    "raw Telegram command received"
                );
                let progress_config = ai_progress.read().await.clone();
                let progress = command_progress_text(&candidate.command, &progress_config);
                tasks.spawn(execute_fast_command(
                    Arc::clone(&session),
                    client.clone(),
                    plugin,
                    Arc::clone(&command_capacity),
                    candidate,
                    progress,
                ));
            }
            update = updates.next() => {
                let update = update.context("Telegram update stream failed")?;
                let Update::NewMessage(message) = update else { continue };
                let outgoing = message.outgoing();
                let self_sender = message.sender().is_some_and(|sender| sender.id() == self_id);
                let saved_messages = message.peer_id() == self_id;
                if !command_belongs_to_self(outgoing, self_sender, saved_messages) { continue; }
                let Some(command) = parse(message.text(), &config.telegram.command_prefixes) else { continue };
                let Some(plugin) = router.plugin_for(&command.name) else { continue };
                let received_at_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i128;
                let telegram_age_ms = received_at_ms
                    .saturating_sub(message.date().timestamp_millis() as i128);
                info!(
                    message_id = message.id(),
                    command = command.name,
                    outgoing,
                    self_sender,
                    saved_messages,
                    telegram_age_ms,
                    "Telegram command received"
                );
                let key = (message.peer_id(), message.id());
                if !remember_command(&mut started_commands, key) {
                    info!(
                        message_id = message.id(),
                        command = command.name,
                        "ordered command duplicate skipped"
                    );
                    continue;
                }
                let queued_at = Instant::now();
                let message_id = message.id();
                let message = message.into_inner();
                let client = client.clone();
                let capacity = Arc::clone(&command_capacity);
                tasks.spawn(async move {
                    let _permit = match capacity.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => return,
                    };
                    info!(
                        message_id,
                        queue_ms = queued_at.elapsed().as_millis(),
                        "command worker acquired"
                    );
                    let context = CommandContext { client: client.clone(), message: message.clone(), command };
                    if let Err(error) = plugin.handle(context).await {
                        error!(plugin = plugin.name(), %error, "command failed");
                        let text = format!("❌ {}", user_facing_error(&error));
                        if let Err(edit_error) = replace_with_chunks(&client, &message, &text).await {
                            error!(%edit_error, "failed to show command error");
                        }
                    }
                });
            }
        }
    }

    updates
        .sync_update_state()
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    handle.quit();
    let _ = pool_task.await;
    let _ = raw_updates_task.await;
    while tasks.join_next().await.is_some() {}
    Ok(())
}

fn command_belongs_to_self(outgoing: bool, self_sender: bool, saved_messages: bool) -> bool {
    outgoing || self_sender || saved_messages
}

fn user_facing_error(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    let mut value = message.chars().take(800).collect::<String>();
    if message.chars().count() > 800 {
        value.push('…');
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        AiProgressConfig, Command, PeerId, UpdatesLike, command_belongs_to_self,
        command_progress_text, raw_command_candidates, tl,
    };

    fn short_message(out: bool, user_id: i64, text: &str) -> UpdatesLike {
        UpdatesLike::Updates(tl::enums::Updates::UpdateShortMessage(
            tl::types::UpdateShortMessage {
                out,
                mentioned: false,
                media_unread: false,
                silent: false,
                id: 123,
                user_id,
                message: text.to_owned(),
                pts: 1,
                pts_count: 1,
                date: 1,
                fwd_from: None,
                via_bot_id: None,
                reply_to: None,
                entities: None,
                ttl_period: None,
            },
        ))
    }

    #[test]
    fn accepts_self_commands_even_when_telegram_does_not_mark_them_outgoing() {
        assert!(command_belongs_to_self(false, true, false));
        assert!(command_belongs_to_self(false, false, true));
        assert!(command_belongs_to_self(true, false, false));
        assert!(!command_belongs_to_self(false, false, false));
    }

    #[test]
    fn raw_fast_path_extracts_only_self_commands() {
        let self_id = PeerId::user(42).unwrap();
        let prefixes = vec![".".to_owned(), "。".to_owned()];
        let candidates =
            raw_command_candidates(&short_message(true, 42, "。AI rust"), self_id, &prefixes);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].command.name, "ai");
        assert_eq!(candidates[0].command.raw_args, "rust");

        assert!(
            raw_command_candidates(&short_message(false, 99, ".ai ignored"), self_id, &prefixes,)
                .is_empty()
        );
    }

    #[test]
    fn fast_progress_redacts_keys_and_labels_history() {
        let searching = AiProgressConfig {
            default_search: true,
            searching: "🔎 自定义搜索进度".to_owned(),
            thinking: "💭 自定义思考进度".to_owned(),
        };
        let key_command = Command {
            prefix: ".".to_owned(),
            name: "ai".to_owned(),
            raw_args: "config key very-secret".to_owned(),
            args: vec![
                "config".to_owned(),
                "key".to_owned(),
                "very-secret".to_owned(),
            ],
        };
        assert_eq!(
            command_progress_text(&key_command, &searching),
            Some("🔐 正在安全更新 AI 配置…".to_owned())
        );

        let context_command = Command {
            prefix: ".".to_owned(),
            name: "ai".to_owned(),
            raw_args: "context 6".to_owned(),
            args: vec!["context".to_owned(), "6".to_owned()],
        };
        assert_eq!(command_progress_text(&context_command, &searching), None);

        let search_command = Command {
            prefix: ".".to_owned(),
            name: "ai".to_owned(),
            raw_args: "search rust".to_owned(),
            args: vec!["search".to_owned(), "rust".to_owned()],
        };
        assert_eq!(
            command_progress_text(&search_command, &searching),
            Some("🔎 自定义搜索进度".to_owned())
        );

        let history_command = Command {
            prefix: ".".to_owned(),
            name: "q".to_owned(),
            raw_args: "history 1".to_owned(),
            args: vec!["history".to_owned(), "1".to_owned()],
        };
        assert_eq!(
            command_progress_text(&history_command, &searching),
            Some("🗂️ 正在读取语录存档…".to_owned())
        );
    }
}
