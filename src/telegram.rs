use anyhow::{Context, Result};
use grammers_client::Client;
use grammers_client::message::{InputMessage, Message};
use grammers_session::types::PeerRef;
use grammers_tl_types as tl;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

const TELEGRAM_TEXT_LIMIT: usize = 3900;
const TELEGRAM_RICH_TEXT_LIMIT: usize = 3800;

pub async fn edit_progress(message: &Message, text: &str) -> Result<()> {
    match message
        .edit(InputMessage::new().text(text).link_preview(false))
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if is_message_not_modified(&error.to_string()) => Ok(()),
        Err(error) => Err(error).with_context(|| "failed to edit command progress"),
    }
}

fn is_message_not_modified(error: &str) -> bool {
    error.contains("MESSAGE_NOT_MODIFIED")
}

pub async fn replace_with_chunks(client: &Client, message: &Message, text: &str) -> Result<()> {
    let chunks = split_telegram_text(text, TELEGRAM_TEXT_LIMIT);
    let first = chunks
        .first()
        .map(String::as_str)
        .unwrap_or("(empty response)");
    message
        .edit(InputMessage::new().text(first).link_preview(true))
        .await
        .with_context(|| "failed to edit command response")?;

    if chunks.len() > 1 {
        let peer = require_peer_ref(message).await?;
        for chunk in chunks.iter().skip(1) {
            client
                .send_message(peer, InputMessage::new().text(chunk).link_preview(true))
                .await
                .with_context(|| "failed to send response continuation")?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RichEntityKind {
    Bold,
    Italic,
    Strike,
    Code,
    Pre(String),
    TextUrl(String),
    Blockquote { collapsed: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RichEntity {
    offset: usize,
    length: usize,
    kind: RichEntityKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RichText {
    text: String,
    entities: Vec<RichEntity>,
}

impl RichText {
    fn push(&mut self, value: &str) {
        self.text.push_str(value);
    }

    fn offset(&self) -> usize {
        utf16_len(&self.text)
    }

    fn push_entity(&mut self, start: usize, kind: RichEntityKind) {
        let length = self.offset().saturating_sub(start);
        if length > 0 {
            self.entities.push(RichEntity {
                offset: start,
                length,
                kind,
            });
        }
    }

    fn append(&mut self, mut other: RichText) {
        let shift = self.offset();
        self.text.push_str(&other.text);
        for entity in &mut other.entities {
            entity.offset += shift;
        }
        self.entities.extend(other.entities);
    }

    fn ensure_newlines(&mut self, wanted: usize) {
        let existing = self.text.chars().rev().take_while(|ch| *ch == '\n').count();
        for _ in existing..wanted {
            self.text.push('\n');
        }
    }

    fn trim_end(&mut self) {
        let byte_len = self.text.trim_end().len();
        self.text.truncate(byte_len);
        let limit = self.offset();
        for entity in &mut self.entities {
            entity.length = entity.length.min(limit.saturating_sub(entity.offset));
        }
        self.entities.retain(|entity| entity.length > 0);
    }

    /// Build a Telegram input message when the rich text fits in a single message.
    /// Operational checks use this to ask Telegram itself to validate the entities.
    pub fn input_message(&self) -> InputMessage {
        let range = split_telegram_ranges(&self.text, TELEGRAM_RICH_TEXT_LIMIT)
            .first()
            .copied()
            .unwrap_or_default();
        InputMessage::new()
            .text(&self.text[range.byte_start..range.byte_end])
            .fmt_entities(entities_for_range(self, range))
            .link_preview(false)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntityClass {
    Bold,
    Italic,
    Strike,
    Pre,
    Link,
}

impl RichEntityKind {
    fn class(&self) -> EntityClass {
        match self {
            Self::Bold => EntityClass::Bold,
            Self::Italic => EntityClass::Italic,
            Self::Strike => EntityClass::Strike,
            Self::Code => EntityClass::Pre,
            Self::Pre(_) => EntityClass::Pre,
            Self::TextUrl(_) => EntityClass::Link,
            Self::Blockquote { .. } => EntityClass::Pre,
        }
    }
}

#[derive(Clone, Debug)]
struct OpenEntity {
    start: usize,
    kind: RichEntityKind,
}

#[derive(Clone, Debug)]
struct ListState {
    ordered: bool,
    next: u64,
}

struct MarkdownBuilder {
    rich: RichText,
    open: Vec<OpenEntity>,
    lists: Vec<ListState>,
    quote_depth: usize,
    table_cell_seen: bool,
}

impl MarkdownBuilder {
    fn new() -> Self {
        Self {
            rich: RichText::default(),
            open: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            table_cell_seen: false,
        }
    }

    fn open(&mut self, kind: RichEntityKind) {
        self.open.push(OpenEntity {
            start: self.rich.offset(),
            kind,
        });
    }

    fn close(&mut self, class: EntityClass) {
        let Some(index) = self
            .open
            .iter()
            .rposition(|entity| entity.kind.class() == class)
        else {
            return;
        };
        let entity = self.open.remove(index);
        self.rich.push_entity(entity.start, entity.kind);
    }

    fn line_break(&mut self) {
        self.rich.ensure_newlines(1);
        if self.quote_depth > 0 {
            self.rich.push(&"┃ ".repeat(self.quote_depth.min(3)));
        }
    }

    fn block_break(&mut self) {
        self.rich.ensure_newlines(2);
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                if !self.rich.text.is_empty() {
                    self.block_break();
                }
                let marker = match level as u8 {
                    2 => "▌",
                    3 => "• ",
                    _ => "",
                };
                self.rich.push(marker);
                self.open(RichEntityKind::Bold);
            }
            Tag::BlockQuote(_) => {
                if !self.rich.text.is_empty() {
                    self.block_break();
                }
                self.quote_depth += 1;
                self.rich.push(&"┃ ".repeat(self.quote_depth.min(3)));
            }
            Tag::CodeBlock(kind) => {
                if !self.rich.text.is_empty() {
                    self.block_break();
                }
                let language = match kind {
                    CodeBlockKind::Indented => String::new(),
                    CodeBlockKind::Fenced(language) => language
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                };
                self.open(RichEntityKind::Pre(language));
            }
            Tag::List(first) => self.lists.push(ListState {
                ordered: first.is_some(),
                next: first.unwrap_or(1),
            }),
            Tag::Item => {
                self.rich.ensure_newlines(1);
                let depth = self.lists.len().saturating_sub(1).min(2);
                self.rich.push(&"  ".repeat(depth));
                if let Some(list) = self.lists.last_mut() {
                    if list.ordered {
                        let marker = format!("{}. ", list.next);
                        list.next += 1;
                        self.rich.push(&marker);
                    } else {
                        self.rich.push("• ");
                    }
                }
            }
            Tag::Emphasis | Tag::Superscript | Tag::Subscript => self.open(RichEntityKind::Italic),
            Tag::Strong | Tag::DefinitionListTitle | Tag::TableHead => {
                self.open(RichEntityKind::Bold)
            }
            Tag::Strikethrough => self.open(RichEntityKind::Strike),
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                let url = dest_url.to_string();
                if is_safe_link(&url) {
                    self.open(RichEntityKind::TextUrl(url));
                }
            }
            Tag::TableRow => self.table_cell_seen = false,
            Tag::TableCell => {
                if self.table_cell_seen {
                    self.rich.push(" │ ");
                }
                self.table_cell_seen = true;
            }
            Tag::FootnoteDefinition(label) => {
                self.block_break();
                self.rich.push(&format!("[{label}] "));
            }
            Tag::DefinitionListDefinition => self.rich.push("  "),
            Tag::HtmlBlock | Tag::DefinitionList | Tag::Table(_) | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if self.lists.is_empty() {
                    self.block_break();
                }
            }
            TagEnd::Heading(_) => {
                self.close(EntityClass::Bold);
                self.block_break();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.block_break();
            }
            TagEnd::CodeBlock => {
                self.close(EntityClass::Pre);
                self.block_break();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.rich
                    .ensure_newlines(if self.lists.is_empty() { 2 } else { 1 });
            }
            TagEnd::Item => self.rich.ensure_newlines(1),
            TagEnd::Emphasis | TagEnd::Superscript | TagEnd::Subscript => {
                self.close(EntityClass::Italic)
            }
            TagEnd::Strong | TagEnd::DefinitionListTitle | TagEnd::TableHead => {
                self.close(EntityClass::Bold)
            }
            TagEnd::Strikethrough => self.close(EntityClass::Strike),
            TagEnd::Link | TagEnd::Image => self.close(EntityClass::Link),
            TagEnd::TableRow => self.rich.ensure_newlines(1),
            TagEnd::Table => self.block_break(),
            TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::TableCell
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn finish(mut self) -> RichText {
        while let Some(entity) = self.open.pop() {
            self.rich.push_entity(entity.start, entity.kind);
        }
        self.rich.trim_end();
        self.rich
    }
}

pub fn telegram_markdown(markdown: &str) -> RichText {
    let mut options = Options::empty();
    options.insert(
        Options::ENABLE_GFM
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS,
    );
    let mut builder = MarkdownBuilder::new();
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => builder.start(tag),
            Event::End(tag) => builder.end(tag),
            Event::Text(text) => builder.rich.push(&text),
            Event::Code(code) | Event::InlineMath(code) => {
                let start = builder.rich.offset();
                builder.rich.push(&code);
                builder.rich.push_entity(start, RichEntityKind::Code);
            }
            Event::DisplayMath(code) => {
                let start = builder.rich.offset();
                builder.rich.push(&code);
                builder
                    .rich
                    .push_entity(start, RichEntityKind::Pre(String::new()));
                builder.block_break();
            }
            Event::Html(html) | Event::InlineHtml(html) => builder.rich.push(&html),
            Event::FootnoteReference(label) => builder.rich.push(&format!("[{label}]")),
            Event::SoftBreak | Event::HardBreak => builder.line_break(),
            Event::Rule => {
                builder.block_break();
                builder.rich.push("────────────────");
                builder.block_break();
            }
            Event::TaskListMarker(checked) => {
                builder.rich.push(if checked { "☑ " } else { "☐ " });
            }
        }
    }
    builder.finish()
}

pub fn ai_rich_response(
    question: &str,
    answer_markdown: &str,
    provider: &str,
    collapse_threshold: usize,
) -> RichText {
    let mut rich = RichText::default();
    let q_label = rich.offset();
    rich.push("Q:");
    rich.push_entity(q_label, RichEntityKind::Bold);
    rich.push("\n");
    let q_start = rich.offset();
    rich.push(question.trim());
    let question_length = rich.offset().saturating_sub(q_start);
    if question_length >= collapse_threshold {
        rich.entities.push(RichEntity {
            offset: q_start,
            length: question_length,
            kind: RichEntityKind::Blockquote { collapsed: true },
        });
    } else {
        rich.push_entity(q_start, RichEntityKind::Blockquote { collapsed: false });
    }

    rich.push("\n\n");
    let a_label = rich.offset();
    rich.push("A:");
    rich.push_entity(a_label, RichEntityKind::Bold);
    rich.push("\n");
    let answer_start = rich.offset();
    rich.append(telegram_markdown(answer_markdown));
    let answer_length = rich.offset().saturating_sub(answer_start);
    if answer_length >= collapse_threshold {
        rich.entities.push(RichEntity {
            offset: answer_start,
            length: answer_length,
            kind: RichEntityKind::Blockquote { collapsed: true },
        });
    }

    rich.push("\n\n");
    let footer_start = rich.offset();
    rich.push(&format!("🍀 Powered by {provider}"));
    rich.push_entity(footer_start, RichEntityKind::Italic);
    rich.trim_end();
    rich
}

pub async fn replace_with_markdown(
    client: &Client,
    message: &Message,
    markdown: &str,
) -> Result<()> {
    replace_with_rich_chunks(client, message, &telegram_markdown(markdown)).await
}

pub async fn replace_with_rich_chunks(
    client: &Client,
    message: &Message,
    rich: &RichText,
) -> Result<()> {
    let ranges = split_telegram_ranges(&rich.text, TELEGRAM_RICH_TEXT_LIMIT);
    let first = ranges.first().copied().unwrap_or_default();
    let first_text = &rich.text[first.byte_start..first.byte_end];
    message
        .edit(
            InputMessage::new()
                .text(first_text)
                .fmt_entities(entities_for_range(rich, first))
                .link_preview(false),
        )
        .await
        .with_context(|| "failed to edit rich command response")?;

    if ranges.len() > 1 {
        let peer = require_peer_ref(message).await?;
        for range in ranges.iter().copied().skip(1) {
            client
                .send_message(
                    peer,
                    InputMessage::new()
                        .text(&rich.text[range.byte_start..range.byte_end])
                        .fmt_entities(entities_for_range(rich, range))
                        .link_preview(false)
                        .reply_to(Some(message.id())),
                )
                .await
                .with_context(|| "failed to send rich response continuation")?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct TextRange {
    byte_start: usize,
    byte_end: usize,
    utf16_start: usize,
    utf16_end: usize,
}

fn split_telegram_ranges(text: &str, max_utf16: usize) -> Vec<TextRange> {
    if text.is_empty() {
        return vec![TextRange::default()];
    }
    let mut ranges = Vec::new();
    let mut byte_start = 0usize;
    let mut utf16_start = 0usize;
    while byte_start < text.len() {
        let rest = &text[byte_start..];
        if utf16_len(rest) <= max_utf16 {
            ranges.push(TextRange {
                byte_start,
                byte_end: text.len(),
                utf16_start,
                utf16_end: utf16_start + utf16_len(rest),
            });
            break;
        }
        let mut used = 0usize;
        let mut hard_end = 0usize;
        for (index, ch) in rest.char_indices() {
            let next = used + ch.len_utf16();
            if next > max_utf16 {
                break;
            }
            used = next;
            hard_end = index + ch.len_utf8();
        }
        let candidate = &rest[..hard_end];
        let relative_end = candidate
            .rfind("\n\n")
            .map(|index| index + 2)
            .or_else(|| candidate.rfind('\n').map(|index| index + 1))
            .or_else(|| candidate.rfind('。').map(|index| index + '。'.len_utf8()))
            .or_else(|| candidate.rfind(' ').map(|index| index + 1))
            .filter(|index| *index > hard_end / 3)
            .unwrap_or(hard_end);
        let utf16_end = utf16_start + utf16_len(&rest[..relative_end]);
        ranges.push(TextRange {
            byte_start,
            byte_end: byte_start + relative_end,
            utf16_start,
            utf16_end,
        });
        byte_start += relative_end;
        utf16_start = utf16_end;
    }
    ranges
}

fn entities_for_range(rich: &RichText, range: TextRange) -> Vec<tl::enums::MessageEntity> {
    rich.entities
        .iter()
        .filter_map(|entity| {
            let start = entity.offset.max(range.utf16_start);
            let end = (entity.offset + entity.length).min(range.utf16_end);
            if start >= end {
                return None;
            }
            let offset = i32::try_from(start - range.utf16_start).ok()?;
            let length = i32::try_from(end - start).ok()?;
            Some(match &entity.kind {
                RichEntityKind::Bold => tl::types::MessageEntityBold { offset, length }.into(),
                RichEntityKind::Italic => tl::types::MessageEntityItalic { offset, length }.into(),
                RichEntityKind::Strike => tl::types::MessageEntityStrike { offset, length }.into(),
                RichEntityKind::Code => tl::types::MessageEntityCode { offset, length }.into(),
                RichEntityKind::Pre(language) => tl::types::MessageEntityPre {
                    offset,
                    length,
                    language: language.clone(),
                }
                .into(),
                RichEntityKind::TextUrl(url) => tl::types::MessageEntityTextUrl {
                    offset,
                    length,
                    url: url.clone(),
                }
                .into(),
                RichEntityKind::Blockquote { collapsed } => tl::types::MessageEntityBlockquote {
                    offset,
                    length,
                    collapsed: *collapsed,
                }
                .into(),
            })
        })
        .collect()
}

fn is_safe_link(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://") || url.starts_with("tg://")
}

pub async fn require_peer_ref(message: &Message) -> Result<PeerRef> {
    message
        .peer_ref()
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .ok_or_else(|| anyhow::anyhow!("message peer is not available in the session cache"))
}

pub fn split_telegram_text(text: &str, max_utf16: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut rest = text;
    while utf16_len(rest) > max_utf16 {
        let mut used = 0usize;
        let mut hard_end = 0usize;
        for (index, ch) in rest.char_indices() {
            let next = used + ch.len_utf16();
            if next > max_utf16 {
                break;
            }
            used = next;
            hard_end = index + ch.len_utf8();
        }
        let candidate = &rest[..hard_end];
        let split = candidate
            .rfind("\n\n")
            .or_else(|| candidate.rfind('\n'))
            .or_else(|| candidate.rfind('。'))
            .or_else(|| candidate.rfind(' '))
            .filter(|index| *index > hard_end / 3)
            .map(|index| index + rest[index..].chars().next().unwrap().len_utf8())
            .unwrap_or(hard_end);
        chunks.push(rest[..split].trim_end().to_owned());
        rest = rest[split..].trim_start();
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    chunks
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_progress_edit_is_idempotent() {
        assert!(is_message_not_modified(
            "rpc error 400: MESSAGE_NOT_MODIFIED caused by messages.editMessage"
        ));
        assert!(!is_message_not_modified(
            "rpc error 400: MESSAGE_ID_INVALID"
        ));
    }

    #[test]
    fn chunks_respect_utf16_limit() {
        let text = "😀".repeat(3000);
        let chunks = split_telegram_text(&text, 3900);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.encode_utf16().count() <= 3900)
        );
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn markdown_keeps_common_ai_formatting() {
        let rich =
            telegram_markdown("# 标题\n\n- **重点**\n- `code`\n\n```rust\nfn main() {}\n```");
        assert!(rich.text.contains("标题"));
        assert!(rich.text.contains("• 重点"));
        assert!(rich.text.contains("fn main()"));
        assert!(
            rich.entities
                .iter()
                .any(|entity| matches!(entity.kind, RichEntityKind::Bold))
        );
        assert!(
            rich.entities.iter().any(
                |entity| matches!(entity.kind, RichEntityKind::Pre(ref lang) if lang == "rust")
            )
        );
    }

    #[test]
    fn long_ai_answer_uses_expandable_blockquote() {
        let rich = ai_rich_response("问题", &"很长".repeat(500), "Gemini", 600);
        assert!(rich.text.ends_with("🍀 Powered by Gemini"));
        assert!(!rich.text.contains("联网搜索"));
        assert!(!rich.text.contains("普通回答"));
        assert!(
            rich.entities.iter().any(|entity| matches!(
                entity.kind,
                RichEntityKind::Blockquote { collapsed: true }
            ))
        );
    }

    #[test]
    fn long_ai_question_uses_expandable_blockquote() {
        let rich = ai_rich_response(&"question ".repeat(100), "short answer", "Gemini", 600);
        let collapsed = rich
            .entities
            .iter()
            .filter(|entity| matches!(entity.kind, RichEntityKind::Blockquote { collapsed: true }))
            .count();
        assert_eq!(collapsed, 1);
    }

    #[test]
    fn rich_chunks_clip_entities_to_utf16_boundaries() {
        let rich = ai_rich_response("问题", &"😀内容".repeat(2000), "Gemini", 100);
        let ranges = split_telegram_ranges(&rich.text, 500);
        assert!(ranges.len() > 1);
        for range in ranges {
            let chunk_len = range.utf16_end - range.utf16_start;
            assert!(chunk_len <= 500);
            for entity in entities_for_range(&rich, range) {
                let (offset, length) = match entity {
                    tl::enums::MessageEntity::Bold(value) => (value.offset, value.length),
                    tl::enums::MessageEntity::Italic(value) => (value.offset, value.length),
                    tl::enums::MessageEntity::Blockquote(value) => (value.offset, value.length),
                    _ => continue,
                };
                assert!(offset >= 0 && length > 0);
                assert!(offset as usize + length as usize <= chunk_len);
            }
        }
    }
}
