use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use futures_util::StreamExt;
use grammers_client::Client;
use grammers_client::media::Media;
use grammers_client::message::{InputMessage, Message};
use grammers_client::peer::Peer;
use grammers_session::types::PeerId;
use grammers_tl_types as tl;
use image::{GenericImageView, ImageFormat};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::RwLock;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::config::QuoteConfig;
use crate::plugin::{CommandContext, Plugin};
use crate::store::Store;
use crate::telegram::{
    edit_progress, replace_with_chunks, replace_with_markdown, require_peer_ref,
};

const STICKER_SET_SETTING: &str = "quote.sticker_set_short_name";
const HISTORY_ENABLED_SETTING: &str = "quote.history.enabled";
const HISTORY_LIMIT_SETTING: &str = "quote.history.limit";
const MAX_MEDIA_BYTES: u64 = 12 * 1024 * 1024;
const MAX_QUOTE_RESPONSE_BYTES: usize = 12 * 1024 * 1024;
const MAX_HISTORY_PREVIEW_CHARS: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuoteOutput {
    Sticker,
    Image,
    Stories,
}

impl QuoteOutput {
    fn api_type(self) -> &'static str {
        match self {
            Self::Sticker => "quote",
            Self::Image => "image",
            Self::Stories => "stories",
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::Sticker => "webp",
            Self::Image | Self::Stories => "png",
        }
    }

    fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Sticker | Self::Image => (512, 768),
            Self::Stories => (360, 640),
        }
    }

    fn history_kind(self) -> &'static str {
        match self {
            Self::Sticker => "sticker",
            Self::Image => "image",
            Self::Stories => "stories",
        }
    }

    fn from_history_kind(value: &str) -> Result<Self> {
        match value {
            "sticker" => Ok(Self::Sticker),
            "image" => Ok(Self::Image),
            "stories" => Ok(Self::Stories),
            _ => bail!("存档格式无效"),
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Sticker => "贴纸",
            Self::Image => "图片",
            Self::Stories => "故事图",
        }
    }
}

pub async fn check_provider(config: &QuoteConfig) -> Result<()> {
    if !config.enabled {
        bail!("quote plugin is disabled");
    }
    let temporary = tempfile::tempdir()?;
    let store = Arc::new(Store::open(&temporary.path().join("check.db")).await?);
    let plugin = QuotePlugin::new(config.clone(), store)?;
    let payload = json!({
        "type": "quote",
        "format": "webp",
        "backgroundColor": config.background_color,
        "width": 512,
        "height": 768,
        "scale": 2,
        "emojiBrand": "apple",
        "messages": [{
            "from": {
                "id": 1,
                "name": "telebot",
                "first_name": "telebot",
                "last_name": "",
                "username": "",
                "photo": null
            },
            "text": "telebot 中文字体检查：你好，世界！",
            "entities": [],
            "avatar": false,
            "media": null,
            "replyMessage": null
        }]
    });
    let bytes = plugin.request_quote(&payload, QuoteOutput::Sticker).await?;
    println!("Quote renderer check passed ({} bytes)", bytes.len());
    Ok(())
}

pub struct QuotePlugin {
    config: QuoteConfig,
    store: Arc<Store>,
    http: reqwest::Client,
    avatar_cache: Arc<RwLock<HashMap<PeerId, Value>>>,
}

impl QuotePlugin {
    pub fn new(config: QuoteConfig, store: Arc<Store>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .user_agent(concat!("telebot/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to construct quote HTTP client")?;
        Ok(Self {
            config,
            store,
            http,
            avatar_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    async fn sticker_set_name(&self) -> Result<String> {
        Ok(self
            .store
            .get_setting(STICKER_SET_SETTING)
            .await?
            .unwrap_or_else(|| self.config.sticker_set_short_name.clone()))
    }

    async fn history_enabled(&self) -> Result<bool> {
        match self.store.get_setting(HISTORY_ENABLED_SETTING).await? {
            Some(value) => parse_history_enabled(&value),
            None => Ok(self.config.history_enabled),
        }
    }

    async fn history_limit(&self) -> Result<usize> {
        let limit = match self.store.get_setting(HISTORY_LIMIT_SETTING).await? {
            Some(value) => value.parse::<usize>().context("语录存档数量必须是数字")?,
            None => self.config.history_limit,
        };
        if !(1..=500).contains(&limit) {
            bail!("语录存档数量必须在 1 到 500 之间");
        }
        Ok(limit)
    }

    async fn archive_quote(&self, output: QuoteOutput, preview: &str, bytes: &[u8]) -> Result<()> {
        if !self.history_enabled().await? {
            return Ok(());
        }
        let limit = self.history_limit().await?;
        let id = self
            .store
            .add_quote_history(
                output.history_kind(),
                preview,
                bytes,
                limit,
                self.config.history_max_bytes,
            )
            .await?;
        info!(
            history_id = id,
            output = output.history_kind(),
            bytes = bytes.len(),
            "quote archived"
        );
        Ok(())
    }

    async fn history_command(&self, context: &CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            let entries = self.store.quote_history(10).await?;
            if entries.is_empty() {
                return replace_with_chunks(
                    &context.client,
                    &context.message,
                    "🗂️ 暂无语录存档。启用后，新生成的 `.q` 会自动保存。",
                )
                .await;
            }
            let mut lines = vec!["🗂️ 最近的语录存档".to_owned()];
            for entry in entries {
                let output = QuoteOutput::from_history_kind(&entry.output)?;
                lines.push(format!(
                    "#{} · {} · {}\n{}",
                    entry.id,
                    output.display_name(),
                    format_byte_size(entry.byte_len),
                    entry.preview
                ));
            }
            lines.push("取回：.q history <ID>".to_owned());
            return replace_with_chunks(&context.client, &context.message, &lines.join("\n\n"))
                .await;
        }
        if args.len() != 1 {
            bail!("用法：.q history [ID]");
        }
        let id = args[0].parse::<i64>().context("存档 ID 必须是数字")?;
        if id <= 0 {
            bail!("存档 ID 必须大于 0");
        }
        edit_progress(&context.message, "🗂️ 正在读取语录存档…").await?;
        let entry = self
            .store
            .quote_history_entry(id)
            .await?
            .ok_or_else(|| anyhow!("未找到语录存档 #{id}"))?;
        let output = QuoteOutput::from_history_kind(&entry.output)?;
        match output {
            QuoteOutput::Sticker => {
                send_sticker_bytes(&context.client, &context.message, &entry.media, None).await?
            }
            QuoteOutput::Image | QuoteOutput::Stories => {
                send_photo_bytes(&context.client, &context.message, &entry.media, None).await?
            }
        }
        context.message.delete().await?;
        info!(
            history_id = entry.id,
            created_at = entry.created_at,
            output = entry.output,
            preview = entry.preview,
            "quote history resent"
        );
        Ok(())
    }

    async fn generate_quote(
        &self,
        context: &CommandContext,
        include_replies: bool,
        output: QuoteOutput,
        count: usize,
    ) -> Result<()> {
        if count == 0 || count > self.config.max_messages {
            bail!("消息数必须在 1-{} 之间", self.config.max_messages);
        }
        edit_progress(&context.message, "🖼️ 正在生成语录贴纸…").await?;
        let replied = context
            .message
            .get_reply()
            .await?
            .ok_or_else(|| anyhow!("请回复一条消息"))?;

        let peer = require_peer_ref(&context.message).await?;
        let mut iterator = context
            .client
            .iter_messages(peer)
            .offset_id(replied.id() - 1)
            .reverse(true)
            .limit(count + 1);
        let mut messages = Vec::with_capacity(count);
        while let Some(message) = iterator.next().await? {
            if message.id() == context.message.id() {
                continue;
            }
            messages.push(message);
            if messages.len() == count {
                break;
            }
        }
        if messages.is_empty() {
            bail!("未找到可生成语录的消息");
        }

        let temporary =
            tempfile::tempdir().context("failed to create quote temporary directory")?;
        let selected = selected_reply_content(&context.message);
        let mut previous_sender = None;
        let mut avatar_count = 0usize;
        let mut payload_messages = Vec::with_capacity(messages.len());
        let mut preview_lines = Vec::with_capacity(messages.len());
        for (index, message) in messages.iter().enumerate() {
            let sender = sender_view(message);
            let sender_id = sender.id;
            let show_avatar = previous_sender != Some(sender_id);
            previous_sender = Some(sender_id);
            let photo = if show_avatar {
                self.download_avatar(&context.client, message.sender(), &temporary, index)
                    .await
            } else {
                None
            };
            avatar_count += usize::from(photo.is_some());
            let media = download_message_media(&context.client, message, &temporary, index).await;
            let (text, entities) = if index == 0 {
                selected.clone().unwrap_or_else(|| {
                    (
                        message.text().to_owned(),
                        convert_entities(message.fmt_entities()),
                    )
                })
            } else {
                (
                    message.text().to_owned(),
                    convert_entities(message.fmt_entities()),
                )
            };
            preview_lines.push(format!(
                "{}：{}",
                sender.display_name,
                quote_preview_line(&text)
            ));

            let reply_message = if include_replies {
                build_reply_block(message).await
            } else {
                None
            };
            payload_messages.push(json!({
                "from": {
                    "id": sender_id,
                    "name": if show_avatar { sender.display_name } else { String::new() },
                    "first_name": if show_avatar { sender.first_name } else { String::new() },
                    "last_name": if show_avatar { sender.last_name } else { String::new() },
                    "username": if show_avatar { sender.username } else { String::new() },
                    "photo": photo,
                },
                "text": text,
                "entities": entities,
                "avatar": show_avatar,
                "media": media,
                "replyMessage": reply_message,
                "forward": forward_label(message),
            }));
        }

        let (width, height) = output.dimensions();
        let payload = json!({
            "type": output.api_type(),
            "format": output.format(),
            "backgroundColor": self.config.background_color,
            "width": width,
            "height": height,
            "scale": 2,
            "emojiBrand": "apple",
            "messages": payload_messages,
        });
        let bytes = self.request_quote(&payload, output).await?;
        match output {
            QuoteOutput::Sticker => {
                send_sticker_bytes(
                    &context.client,
                    &context.message,
                    &bytes,
                    Some(replied.id()),
                )
                .await?
            }
            QuoteOutput::Image | QuoteOutput::Stories => {
                send_photo_bytes(
                    &context.client,
                    &context.message,
                    &bytes,
                    Some(replied.id()),
                )
                .await?
            }
        }
        let preview = quote_history_preview(&preview_lines);
        if let Err(error) = self.archive_quote(output, &preview, &bytes).await {
            warn!(%error, "failed to archive quote; generated media was still sent");
        }
        context.message.delete().await?;
        info!(
            messages = messages.len(),
            avatars = avatar_count,
            bytes = bytes.len(),
            "quote sticker generated"
        );
        Ok(())
    }

    async fn download_avatar(
        &self,
        client: &Client,
        sender: Option<&Peer>,
        temporary: &TempDir,
        index: usize,
    ) -> Option<Value> {
        let sender = sender?;
        let peer_id = sender.id();
        if let Some(cached) = self.avatar_cache.read().await.get(&peer_id).cloned() {
            return Some(cached);
        }

        let peer_ref = match sender.to_ref().await {
            Ok(Some(peer)) => peer,
            Ok(None) => {
                warn!(%peer_id, "quote avatar peer has no cached authority; using ambient reference");
                peer_id.to_ambient_ref()
            }
            Err(error) => {
                warn!(%error, %peer_id, "failed to resolve quote avatar peer reference");
                peer_id.to_ambient_ref()
            }
        };
        let path = temporary.path().join(format!("avatar-{index}.jpg"));
        let mut downloaded = false;

        let mut photo = match sender.photo(false).await {
            Ok(photo) => photo,
            Err(error) => {
                warn!(%error, %peer_id, "failed to inspect quote sender avatar");
                None
            }
        };
        if photo.is_none() {
            match client.resolve_peer(peer_ref).await {
                Ok(peer) => match peer.photo(false).await {
                    Ok(resolved) => photo = resolved,
                    Err(error) => {
                        warn!(%error, %peer_id, "failed to inspect refreshed quote sender avatar")
                    }
                },
                Err(error) => warn!(%error, %peer_id, "failed to refresh quote sender"),
            }
        }
        if let Some(photo) = photo {
            match client.download_media(&photo, &path).await {
                Ok(()) => downloaded = true,
                Err(error) => warn!(%error, %peer_id, "failed to download quote sender avatar"),
            }
        }

        if !downloaded {
            let mut photos = client.iter_profile_photos(peer_ref);
            match photos.next().await {
                Ok(Some(photo)) => match client.download_media(&photo, &path).await {
                    Ok(()) => downloaded = true,
                    Err(error) => {
                        warn!(%error, %peer_id, "failed to download quote profile photo fallback")
                    }
                },
                Ok(None) => info!(%peer_id, "quote sender has no profile avatar"),
                Err(error) => warn!(%error, %peer_id, "failed to list quote profile photos"),
            }
        }
        if !downloaded {
            return None;
        }

        let value = file_data_url(&path, "image/jpeg")
            .await
            .map(|url| json!({"url": url}))?;
        let mut cache = self.avatar_cache.write().await;
        if cache.len() >= 128 && !cache.contains_key(&peer_id) {
            cache.clear();
        }
        cache.insert(peer_id, value.clone());
        info!(%peer_id, "quote sender avatar cached");
        Some(value)
    }

    async fn request_quote(&self, payload: &Value, output: QuoteOutput) -> Result<Vec<u8>> {
        let limit = Duration::from_secs(self.config.timeout_seconds);
        let mut last_error = None;
        let endpoint = quote_endpoint(&self.config.api_url, output);
        for attempt in 0..2 {
            let request = async {
                let response = self
                    .http
                    .post(&endpoint)
                    .header("content-type", "application/json")
                    .json(payload)
                    .send()
                    .await?;
                let status = response.status();
                if response
                    .content_length()
                    .is_some_and(|size| size > MAX_QUOTE_RESPONSE_BYTES as u64)
                {
                    bail!("quote API response is too large");
                }
                let mut stream = response.bytes_stream();
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    if bytes.len() + chunk.len() > MAX_QUOTE_RESPONSE_BYTES {
                        bail!("quote API response is too large");
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok::<_, anyhow::Error>((status, bytes))
            };
            let result = timeout(limit, request).await;
            match result {
                Err(_) => {
                    last_error = Some(anyhow!("quote API timed out after {}s", limit.as_secs()))
                }
                Ok(Err(error)) => last_error = Some(error),
                Ok(Ok((status, bytes))) => {
                    if !status.is_success() {
                        let retryable =
                            status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                        last_error = Some(anyhow!("quote API returned HTTP {}", status.as_u16()));
                        if !retryable {
                            break;
                        }
                    } else {
                        return decode_quote_response(&bytes, output);
                    }
                }
            }
            if attempt == 0 {
                sleep(Duration::from_millis(300)).await;
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("quote API failed")))
    }

    async fn save_to_sticker_set(&self, context: &CommandContext) -> Result<()> {
        let set_name = self.sticker_set_name().await?;
        if set_name.trim().is_empty() {
            bail!("尚未配置贴纸包，请先使用 .q config sticker <名称>");
        }
        let replied = context
            .message
            .get_reply()
            .await?
            .ok_or_else(|| anyhow!("请回复一张贴纸或图片"))?;
        edit_progress(&context.message, "📦 正在保存到贴纸包…").await?;
        let item = prepare_sticker_item(&context.client, &replied).await?;
        let set = tl::types::InputStickerSetShortName {
            short_name: set_name.clone(),
        };
        let exists = match context
            .client
            .invoke(&tl::functions::messages::GetStickerSet {
                stickerset: set.clone().into(),
                hash: 0,
            })
            .await
        {
            Ok(_) => true,
            Err(error) if error.to_string().contains("STICKERSET_INVALID") => false,
            Err(error) => return Err(error.into()),
        };

        if exists {
            context
                .client
                .invoke(&tl::functions::stickers::AddStickerToSet {
                    stickerset: set.into(),
                    sticker: item.into(),
                })
                .await?;
            replace_with_chunks(
                &context.client,
                &context.message,
                &format!("✅ 已添加到贴纸包\nhttps://t.me/addstickers/{set_name}"),
            )
            .await?;
        } else {
            context
                .client
                .invoke(&tl::functions::stickers::CreateStickerSet {
                    masks: false,
                    emojis: false,
                    text_color: false,
                    user_id: tl::enums::InputUser::UserSelf,
                    title: set_name.clone(),
                    short_name: set_name.clone(),
                    thumb: None,
                    stickers: vec![item.into()],
                    software: Some(format!("telebot/{}", env!("CARGO_PKG_VERSION"))),
                })
                .await?;
            replace_with_chunks(
                &context.client,
                &context.message,
                &format!("✅ 已创建贴纸包并添加首张贴纸\nhttps://t.me/addstickers/{set_name}"),
            )
            .await?;
        }
        Ok(())
    }

    async fn config_command(&self, context: &CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            let name = self.sticker_set_name().await?;
            let value = if name.is_empty() {
                "（未设置）".to_owned()
            } else {
                name.clone()
            };
            let link = if name.is_empty() {
                String::new()
            } else {
                format!("\n链接：https://t.me/addstickers/{name}")
            };
            let enabled = self.history_enabled().await?;
            let limit = self.history_limit().await?;
            let entries = self.store.quote_history(limit).await?;
            let used_bytes = entries.iter().map(|entry| entry.byte_len).sum::<usize>();
            return replace_with_chunks(
                &context.client,
                &context.message,
                &format!(
                    "📋 语录配置\n\n贴纸包：{value}{link}\n存档：{}\n存档数量：{}/{}\n存档空间：{}/{}\n\n设置贴纸包：.q config sticker <名称>\n开关存档：.q config history on|off\n存档上限：.q config history limit <1-500>",
                    if enabled { "开启" } else { "关闭" },
                    entries.len(),
                    limit,
                    format_byte_size(used_bytes),
                    format_byte_size(self.config.history_max_bytes),
                ),
            )
            .await;
        }
        let key = args[0].to_ascii_lowercase();
        match key.as_str() {
            "sticker" | "stickerset" | "set" => {
                let name = args[1..].join("_");
                validate_sticker_set_name(&name)?;
                self.store.set_setting(STICKER_SET_SETTING, &name).await?;
                replace_with_chunks(
                    &context.client,
                    &context.message,
                    &format!("✅ 贴纸包已设置为 {name}\nhttps://t.me/addstickers/{name}"),
                )
                .await
            }
            "history" | "archive" => match args.get(1).map(|value| value.to_ascii_lowercase()) {
                Some(value) if matches!(value.as_str(), "on" | "off" | "true" | "false") => {
                    if args.len() != 2 {
                        bail!("用法：.q config history on|off");
                    }
                    let enabled = parse_history_enabled(&value)?;
                    self.store
                        .set_setting(
                            HISTORY_ENABLED_SETTING,
                            if enabled { "true" } else { "false" },
                        )
                        .await?;
                    replace_with_chunks(
                        &context.client,
                        &context.message,
                        if enabled {
                            "✅ Q 历史存档已开启；从下一张新生成的语录开始保存"
                        } else {
                            "✅ Q 历史存档已关闭；已有存档仍可取回"
                        },
                    )
                    .await
                }
                Some(value) if value == "limit" => {
                    if args.len() != 3 {
                        bail!("用法：.q config history limit <1-500>");
                    }
                    let limit = args[2].parse::<usize>().context("语录存档数量必须是数字")?;
                    if !(1..=500).contains(&limit) {
                        bail!("语录存档数量必须在 1 到 500 之间");
                    }
                    self.store
                        .set_setting(HISTORY_LIMIT_SETTING, &limit.to_string())
                        .await?;
                    replace_with_chunks(
                        &context.client,
                        &context.message,
                        &format!("✅ Q 历史存档上限已设置为 {limit} 条"),
                    )
                    .await
                }
                _ => bail!("用法：.q config history on|off，或 .q config history limit <1-500>"),
            },
            _ => bail!("未知配置项；可用：sticker、history"),
        }
    }
}

#[async_trait]
impl Plugin for QuotePlugin {
    fn name(&self) -> &'static str {
        "quote"
    }
    fn commands(&self) -> &'static [&'static str] {
        &["q"]
    }

    async fn handle(&self, context: CommandContext) -> Result<()> {
        let args = &context.command.args;
        if args
            .first()
            .is_some_and(|arg| arg.eq_ignore_ascii_case("config"))
        {
            return self.config_command(&context, &args[1..]).await;
        }
        if args
            .first()
            .is_some_and(|arg| matches!(arg.to_ascii_lowercase().as_str(), "history" | "his"))
        {
            return self.history_command(&context, &args[1..]).await;
        }
        if args
            .first()
            .is_some_and(|arg| arg.eq_ignore_ascii_case("s"))
        {
            return self.save_to_sticker_set(&context).await;
        }
        if args
            .first()
            .is_some_and(|arg| matches!(arg.to_ascii_lowercase().as_str(), "help" | "h"))
        {
            return replace_with_markdown(
                &context.client,
                &context.message,
                "# 🖼️ 语录\n\n- `.q [1-5]` — 从回复消息开始生成 WebP 贴纸\n- `.q r [1-5]` — 同时显示消息中的回复引用\n- `.q image [1-5]` — 生成 PNG 图片\n- `.q stories [1-5]` — 生成故事比例 PNG\n- `.q r image [1-5]` — 图片中同时显示回复引用\n- `.q history` — 查看最近存档\n- `.q history <ID>` — 重新发送一条存档\n- `.q s` — 把回复的贴纸或图片保存到贴纸包\n- `.q config` — 查看配置\n\n> 支持 Telegram 的“选择部分文字后回复”。新存档从功能启用后开始，不补录旧 Q。",
            )
            .await;
        }

        let mut cursor = 0usize;
        let include_replies = args
            .get(cursor)
            .is_some_and(|arg| arg.eq_ignore_ascii_case("r"));
        if include_replies {
            cursor += 1;
        }
        let output = match args.get(cursor).map(|arg| arg.to_ascii_lowercase()) {
            Some(value) if value == "webp" => {
                cursor += 1;
                QuoteOutput::Sticker
            }
            Some(value) if matches!(value.as_str(), "image" | "png") => {
                cursor += 1;
                QuoteOutput::Image
            }
            Some(value) if value == "stories" => {
                cursor += 1;
                QuoteOutput::Stories
            }
            _ => QuoteOutput::Sticker,
        };
        let count = args
            .get(cursor)
            .map(|value| value.parse::<usize>().context("消息数必须是数字"))
            .transpose()?
            .unwrap_or(1);
        if args.len() > cursor + usize::from(args.get(cursor).is_some()) {
            bail!("参数过多，请使用 .q help 查看用法");
        }
        self.generate_quote(&context, include_replies, output, count)
            .await
    }
}

fn parse_history_enabled(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        _ => bail!("存档开关必须是 on 或 off"),
    }
}

fn quote_preview_line(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        "[媒体]".to_owned()
    } else {
        text
    }
}

fn quote_history_preview(lines: &[String]) -> String {
    let source = lines.join(" / ");
    let mut preview = source
        .chars()
        .take(MAX_HISTORY_PREVIEW_CHARS)
        .collect::<String>();
    if source.chars().count() > MAX_HISTORY_PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn format_byte_size(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[derive(Clone)]
struct SenderView {
    id: i64,
    display_name: String,
    first_name: String,
    last_name: String,
    username: String,
}

fn sender_view(message: &Message) -> SenderView {
    if let Some(tl::enums::MessageFwdHeader::Header(header)) = message.forward_header()
        && let Some(name) = header
            .from_name
            .or(header.saved_from_name)
            .or(header.post_author)
    {
        return SenderView {
            id: stable_text_id(&name),
            display_name: name.clone(),
            first_name: name,
            last_name: String::new(),
            username: String::new(),
        };
    }
    match message.sender() {
        Some(Peer::User(user)) => {
            let first = user.first_name().unwrap_or("").to_owned();
            let last = user.last_name().unwrap_or("").to_owned();
            let username = user.username().unwrap_or("").to_owned();
            let display_name = format!("{first} {last}").trim().to_owned();
            SenderView {
                id: user
                    .id()
                    .bare_id()
                    .unwrap_or_else(|| stable_text_id(&display_name)),
                display_name,
                first_name: first,
                last_name: last,
                username,
            }
        }
        Some(peer) => {
            let name = peer.name().unwrap_or("未知来源").to_owned();
            SenderView {
                id: peer.id().bare_id().unwrap_or_else(|| stable_text_id(&name)),
                display_name: name.clone(),
                first_name: name,
                last_name: String::new(),
                username: peer.username().unwrap_or("").to_owned(),
            }
        }
        None => SenderView {
            id: stable_text_id("未知来源"),
            display_name: "未知来源".to_owned(),
            first_name: "未知来源".to_owned(),
            last_name: String::new(),
            username: String::new(),
        },
    }
}

fn stable_text_id(value: &str) -> i64 {
    value.bytes().fold(0_i64, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as i64)
    })
}

async fn download_message_media(
    client: &Client,
    message: &Message,
    temporary: &TempDir,
    index: usize,
) -> Option<Value> {
    let media = message.media()?;
    let path = temporary.path().join(format!("media-{index}.bin"));
    let (mime, result) = match &media {
        Media::Photo(photo) => ("image/jpeg", client.download_media(photo, &path).await),
        Media::Sticker(sticker) => (
            sticker.document.mime_type().unwrap_or("image/webp"),
            client.download_media(&sticker.document, &path).await,
        ),
        Media::Document(document) => {
            let mime = document.mime_type().unwrap_or("application/octet-stream");
            if !mime.starts_with("image/") && !mime.starts_with("video/") {
                return None;
            }
            (mime, client.download_media(document, &path).await)
        }
        _ => return None,
    };
    if let Err(error) = result {
        warn!(%error, "failed to download quote media");
        return None;
    }
    file_data_url(&path, mime)
        .await
        .map(|url| json!({"url": url}))
}

async fn file_data_url(path: &std::path::Path, mime: &str) -> Option<String> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    if metadata.len() == 0 || metadata.len() > MAX_MEDIA_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

async fn build_reply_block(message: &Message) -> Option<Value> {
    let replied = message.get_reply().await.ok()??;
    let sender = sender_view(&replied);
    let (text, entities) = match message.reply_header() {
        Some(tl::enums::MessageReplyHeader::Header(header))
            if header.quote && header.quote_text.is_some() =>
        {
            (
                header.quote_text.unwrap(),
                convert_entities(header.quote_entities.as_ref()),
            )
        }
        _ => (
            replied.text().to_owned(),
            convert_entities(replied.fmt_entities()),
        ),
    };
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "name": sender.display_name,
        "text": text,
        "entities": entities,
        "chatId": sender.id,
    }))
}

fn selected_reply_content(message: &Message) -> Option<(String, Vec<Value>)> {
    match message.reply_header()? {
        tl::enums::MessageReplyHeader::Header(header) if header.quote => header
            .quote_text
            .map(|text| (text, convert_entities(header.quote_entities.as_ref()))),
        _ => None,
    }
}

fn forward_label(message: &Message) -> Option<Value> {
    let tl::enums::MessageFwdHeader::Header(header) = message.forward_header()?;
    let label = header
        .from_name
        .or(header.saved_from_name)
        .or(header.post_author)?;
    Some(json!({"label": label}))
}

fn convert_entities(entities: Option<&Vec<tl::enums::MessageEntity>>) -> Vec<Value> {
    entities
        .into_iter()
        .flatten()
        .filter_map(convert_entity)
        .collect()
}

fn convert_entity(entity: &tl::enums::MessageEntity) -> Option<Value> {
    use tl::enums::MessageEntity as E;
    let basic = |kind: &str, offset: i32, length: i32| json!({"type": kind, "offset": offset, "length": length});
    Some(match entity {
        E::Bold(value) => basic("bold", value.offset, value.length),
        E::Italic(value) => basic("italic", value.offset, value.length),
        E::Underline(value) => basic("underline", value.offset, value.length),
        E::Strike(value) => basic("strikethrough", value.offset, value.length),
        E::Code(value) => basic("code", value.offset, value.length),
        E::Pre(value) => basic("pre", value.offset, value.length),
        E::Url(value) => basic("url", value.offset, value.length),
        E::Mention(value) => basic("mention", value.offset, value.length),
        E::Hashtag(value) => basic("hashtag", value.offset, value.length),
        E::Cashtag(value) => basic("cashtag", value.offset, value.length),
        E::BotCommand(value) => basic("bot_command", value.offset, value.length),
        E::Email(value) => basic("email", value.offset, value.length),
        E::Phone(value) => basic("phone_number", value.offset, value.length),
        E::Spoiler(value) => basic("spoiler", value.offset, value.length),
        E::TextUrl(value) => json!({
            "type": "text_link", "offset": value.offset, "length": value.length, "url": value.url
        }),
        E::CustomEmoji(value) => json!({
            "type": "custom_emoji", "offset": value.offset, "length": value.length,
            "custom_emoji_id": value.document_id.to_string()
        }),
        E::MentionName(value) => json!({
            "type": "text_mention", "offset": value.offset, "length": value.length,
            "user": {"id": value.user_id}
        }),
        _ => return None,
    })
}

async fn send_sticker_bytes(
    client: &Client,
    command: &Message,
    bytes: &[u8],
    reply_to: Option<i32>,
) -> Result<()> {
    let peer = require_peer_ref(command).await?;
    let webm = is_webm(bytes);
    let extension = if webm { "webm" } else { "webp" };
    let mime = if webm { "video/webm" } else { "image/webp" };
    let mut cursor = Cursor::new(bytes);
    let uploaded = client
        .upload_stream(&mut cursor, bytes.len(), format!("quote.{extension}"))
        .await?;
    let mut attributes = vec![
        tl::types::DocumentAttributeSticker {
            mask: false,
            alt: "📝".to_owned(),
            stickerset: tl::enums::InputStickerSet::Empty,
            mask_coords: None,
        }
        .into(),
        tl::types::DocumentAttributeFilename {
            file_name: format!("quote.{extension}"),
        }
        .into(),
    ];
    if webm {
        attributes.push(
            tl::types::DocumentAttributeVideo {
                round_message: false,
                supports_streaming: false,
                nosound: true,
                duration: 3.0,
                w: 512,
                h: 512,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            }
            .into(),
        );
    } else {
        let (width, height) = image::load_from_memory(bytes)
            .map(|image| image.dimensions())
            .unwrap_or((512, 768));
        attributes.push(
            tl::types::DocumentAttributeImageSize {
                w: width as i32,
                h: height as i32,
            }
            .into(),
        );
    }
    let media = tl::types::InputMediaUploadedDocument {
        nosound_video: webm,
        force_file: false,
        spoiler: false,
        file: uploaded.raw,
        thumb: None,
        mime_type: mime.to_owned(),
        attributes,
        stickers: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    };
    client
        .send_message(peer, InputMessage::new().media(media).reply_to(reply_to))
        .await?;
    Ok(())
}

async fn send_photo_bytes(
    client: &Client,
    command: &Message,
    bytes: &[u8],
    reply_to: Option<i32>,
) -> Result<()> {
    if !is_png(bytes) {
        bail!("quote renderer did not return a PNG image");
    }
    let peer = require_peer_ref(command).await?;
    let mut cursor = Cursor::new(bytes);
    let uploaded = client
        .upload_stream(&mut cursor, bytes.len(), "quote.png".to_owned())
        .await?;
    let media = tl::types::InputMediaUploadedPhoto {
        spoiler: false,
        file: uploaded.raw,
        stickers: None,
        ttl_seconds: None,
        live_photo: false,
        video: None,
    };
    client
        .send_message(peer, InputMessage::new().media(media).reply_to(reply_to))
        .await?;
    Ok(())
}

async fn prepare_sticker_item(
    client: &Client,
    message: &Message,
) -> Result<tl::types::InputStickerSetItem> {
    let media = message
        .media()
        .ok_or_else(|| anyhow!("回复的消息不包含贴纸或图片"))?;
    let document = match media {
        Media::Sticker(sticker) => input_document_from_media(&sticker.document)?,
        Media::Photo(photo) => {
            let temporary = tempfile::tempdir()?;
            let source = temporary.path().join("source.jpg");
            client.download_media(&photo, &source).await?;
            let bytes = tokio::fs::read(&source).await?;
            let (sticker, width, height) = normalize_photo_sticker(&bytes)?;
            upload_sticker_document(client, sticker, "image/png", width, height).await?
        }
        _ => bail!("仅支持贴纸或图片"),
    };
    Ok(tl::types::InputStickerSetItem {
        document,
        emoji: "📝".to_owned(),
        mask_coords: None,
        keywords: None,
    })
}

fn input_document_from_media(
    document: &grammers_client::media::Document,
) -> Result<tl::enums::InputDocument> {
    match document.raw.document.as_ref() {
        Some(tl::enums::Document::Document(value)) => Ok(tl::types::InputDocument {
            id: value.id,
            access_hash: value.access_hash,
            file_reference: value.file_reference.clone(),
        }
        .into()),
        _ => bail!("贴纸文档不可用"),
    }
}

fn normalize_photo_sticker(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory(bytes).context("无法解码图片")?;
    for bound in [512_u32, 448, 384, 320] {
        let resized = image.thumbnail(bound, bound);
        let (width, height) = resized.dimensions();
        let mut cursor = Cursor::new(Vec::new());
        resized.write_to(&mut cursor, ImageFormat::Png)?;
        let encoded = cursor.into_inner();
        if encoded.len() <= 500 * 1024 || bound == 320 {
            return Ok((encoded, width, height));
        }
    }
    unreachable!()
}

async fn upload_sticker_document(
    client: &Client,
    bytes: Vec<u8>,
    mime: &str,
    width: u32,
    height: u32,
) -> Result<tl::enums::InputDocument> {
    let mut cursor = Cursor::new(&bytes);
    let uploaded = client
        .upload_stream(&mut cursor, bytes.len(), "sticker.png".to_owned())
        .await?;
    let media = tl::types::InputMediaUploadedDocument {
        nosound_video: false,
        force_file: false,
        spoiler: false,
        file: uploaded.raw,
        thumb: None,
        mime_type: mime.to_owned(),
        attributes: vec![
            tl::types::DocumentAttributeFilename {
                file_name: "sticker.png".to_owned(),
            }
            .into(),
            tl::types::DocumentAttributeImageSize {
                w: width as i32,
                h: height as i32,
            }
            .into(),
        ],
        stickers: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    };
    let uploaded_media = client
        .invoke(&tl::functions::messages::UploadMedia {
            business_connection_id: None,
            peer: tl::enums::InputPeer::PeerSelf,
            media: media.into(),
        })
        .await?;
    match uploaded_media {
        tl::enums::MessageMedia::Document(document) => match document.document {
            Some(tl::enums::Document::Document(value)) => Ok(tl::types::InputDocument {
                id: value.id,
                access_hash: value.access_hash,
                file_reference: value.file_reference,
            }
            .into()),
            _ => bail!("Telegram did not return an uploaded document"),
        },
        _ => bail!("Telegram did not return document media"),
    }
}

fn validate_sticker_set_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("贴纸包名称长度必须在 1-64 个字符之间");
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        bail!("贴纸包名称只能包含英文字母、数字和下划线");
    }
    Ok(())
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn is_webm(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
}

fn quote_endpoint(base: &str, output: QuoteOutput) -> String {
    let base = base.trim_end_matches('/');
    if let Some(prefix) = base
        .strip_suffix(".webp")
        .or_else(|| base.strip_suffix(".png"))
    {
        return format!("{prefix}.{}", output.format());
    }
    if base.ends_with("/generate") {
        return format!("{base}.{}", output.format());
    }
    base.to_owned()
}

fn decode_quote_response(bytes: &[u8], output: QuoteOutput) -> Result<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > MAX_QUOTE_RESPONSE_BYTES {
        bail!("quote API returned an empty or oversized response");
    }
    if is_webp(bytes) || is_webm(bytes) || is_png(bytes) {
        validate_quote_format(bytes, output)?;
        return Ok(bytes.to_vec());
    }

    let value: Value =
        serde_json::from_slice(bytes).context("quote API returned an unsupported response")?;
    let encoded = value
        .get("image")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("quote API JSON response did not contain an image"))?;
    let encoded = encoded.rsplit_once(',').map_or(encoded, |(_, data)| data);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("quote API returned invalid base64 image data")?;
    if decoded.is_empty() || decoded.len() > MAX_QUOTE_RESPONSE_BYTES {
        bail!("quote API returned an empty or oversized image");
    }
    validate_quote_format(&decoded, output)?;
    Ok(decoded)
}

fn validate_quote_format(bytes: &[u8], output: QuoteOutput) -> Result<()> {
    match output {
        QuoteOutput::Sticker if is_webp(bytes) || is_webm(bytes) => Ok(()),
        QuoteOutput::Image | QuoteOutput::Stories if is_png(bytes) => Ok(()),
        QuoteOutput::Sticker => bail!("quote API did not return WebP/WebM"),
        QuoteOutput::Image | QuoteOutput::Stories => bail!("quote API did not return PNG"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_sticker_set_names() {
        assert!(validate_sticker_set_name("telebot_quotes_1").is_ok());
        assert!(validate_sticker_set_name("bad-name").is_err());
        assert!(validate_sticker_set_name("").is_err());
    }

    #[test]
    fn decodes_json_wrapped_webp() {
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPdata");
        let response = serde_json::to_vec(&json!({
            "image": base64::engine::general_purpose::STANDARD.encode(&webp)
        }))
        .unwrap();
        assert_eq!(
            decode_quote_response(&response, QuoteOutput::Sticker).unwrap(),
            webp
        );
    }

    #[test]
    fn selects_format_specific_local_endpoint() {
        assert_eq!(
            quote_endpoint("http://127.0.0.1:3210/generate", QuoteOutput::Sticker),
            "http://127.0.0.1:3210/generate.webp"
        );
        assert_eq!(
            quote_endpoint("http://127.0.0.1:3210/generate", QuoteOutput::Stories),
            "http://127.0.0.1:3210/generate.png"
        );
    }

    #[test]
    fn quote_history_preview_is_single_line_and_bounded() {
        let long = "一".repeat(MAX_HISTORY_PREVIEW_CHARS + 20);
        let preview = quote_history_preview(&[format!("名字：{long}\n第二行")]);
        assert!(!preview.contains('\n'));
        assert!(preview.chars().count() <= MAX_HISTORY_PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn quote_history_output_round_trips() {
        for output in [
            QuoteOutput::Sticker,
            QuoteOutput::Image,
            QuoteOutput::Stories,
        ] {
            assert_eq!(
                QuoteOutput::from_history_kind(output.history_kind()).unwrap(),
                output
            );
        }
        assert!(QuoteOutput::from_history_kind("unknown").is_err());
    }
}
