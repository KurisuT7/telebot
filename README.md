# telebot

`telebot` is an independent Rust Telegram user client. It is intentionally small: the runtime,
command router, state store, AI provider and quote feature are separate modules, so new commands
can be added without modifying Telegram connection code.

Initial commands:

- `.ai <question>`: use native web search by default.
- Reply to text, a selected quote, a photo or a static sticker with `.ai` to use it for this request.
- `.ai chat <question>` / `.ai search <question>`: force offline or native-search mode.
- `.ai config`: inspect the active provider and API format without revealing its key.
- `.ai config provider <api_format> <name> <base_url>`: change the wire format, display name and endpoint.
- `.ai config key <key>` / `.ai config model <primary> [search_fallback]`: update credentials or models.
- `.ai config prompt|thinking|search|timeout|tokens|collapse|message`: update common AI behavior and progress text immediately without rebuilding.
- `.ai config reload`: reload the server AI/message TOML while keeping SQLite runtime overrides; `.ai config reset` clears those overrides.
- `.ai context <0-20|on|off>`: configure per-chat rolling context; it remains off by default.
- `.ai reset|status|help`: clear the current chat context, inspect configuration, or show the detailed usage and safety guide.
- `.q [1-5]`: generate a quote sticker from the replied message and following messages.
- `.q r [1-5]`, `.q image [1-5]`, `.q stories [1-5]`: include replies or choose PNG layouts.
- `.q history` / `.q history <id>`: list recent archived quotes or resend one by ID.
- `.q s`: add a replied sticker or photo to the configured sticker set.
- `.q config`: inspect quote settings; `history on|off` and `history limit <1-500>` control archiving.

All commands accept any configured prefix. The production default is `.`, `。`, `,`, `，`, `$`,
`!`, and `！`.

Telegram may synchronize commands written by the same account with `outgoing=false`, especially
in Saved Messages. The router accepts a command only when it is outgoing, its sender is the
authorized account, or its peer is Saved Messages. Incoming commands from other users are ignored.
Production enables update catch-up so commands written during a short restart are not lost.

AI answers use native Telegram rich-text entities. Markdown headings, emphasis, code, lists and
links are preserved; both the `Q:` and `A:` bodies use expandable block quotes by default, and
oversized answers are split without breaking UTF-16 entity offsets. Provider, model and context
settings are stored
in the local application database. `.ai config key` is accepted only in Saved Messages, immediately
redacts the command, never echoes the value, and stores it in the owner-protected local database;
otherwise the key continues to come from the server environment.

The AI adapter supports `gemini_interactions`, `openai_chat_completions` and `openai_responses`.
OpenAI-compatible services use only the selected standard wire format: the code contains no
gateway-specific branches. Set `base_url` to the API prefix, such as `https://api.example.com/v1`;
telebot appends `chat/completions` or `responses` unless the URL already ends with that endpoint.

Gemini Interactions and OpenAI Responses support native web search. OpenAI Chat Completions has no
standard web-search tool, so a forced search falls back to a clearly labelled non-search answer.
Native-search primary and fallback models are hedged, and HTTPS citation annotations are
deduplicated and rendered as Telegram links. Image-grounded searches have a separate, longer total
timeout so slow multimodal requests do not change the text-only latency budget.

For a generic Chat Completions endpoint, change these fields in `config.toml` and provide the named
environment variable outside the repository:

```toml
[ai]
provider = "generic-oai"
api_format = "openai_chat_completions"
api_key_env = "TELEBOT_AI_API_KEY"
base_url = "https://api.example.com/v1"
model = "example-model"
search_fallback_model = "example-model"
default_search = false
```

Use `openai_responses` only when the endpoint implements that protocol. It enables the standard
Responses `web_search` tool and requires a search fallback model. Telebot does not infer a protocol
from the provider name, model name or URL.

Frequently adjusted AI values are runtime settings stored in SQLite. Provider, BaseURL, Key, models,
system prompt, thinking level, default-search mode, timeouts, maximum output, quote collapsing and AI
progress messages can be changed from Saved Messages and take effect immediately. The `[messages]`
TOML section supplies server defaults. Concurrency and plugin enable/disable remain startup-only.

## Design

- `src/main.rs`: process lifecycle, Telegram update stream and bounded command workers.
- `src/plugin.rs`: small plugin trait and command router; future features do not touch the connection loop.
- `src/plugins/ai.rs`: protocol-neutral runtime settings, Gemini and OpenAI-compatible adapters, bounded context and native-search fallback.
- `src/plugins/quote.rs`: quote generation, bounded media archive and Telegram sticker-set operations.
- `src/telegram.rs`: Telegram-native Markdown formatting, expandable questions and answers and safe message splitting.
- `src/store.rs`: asynchronous SQLite settings, bounded AI context and quote archive storage.
- `src/session_import.rs`: one-time, offline GramJS StringSession migration.

The quote renderer is the official `LyoSU/quote-api` source pinned to a reviewed commit. It is
self-hosted alongside telebot, bound only to `127.0.0.1:3210`, and runs as an unprivileged read-only container
with Noto CJK fonts. This avoids relying on an unstable public renderer and fixes missing Chinese
glyphs.

## Commands

```text
telebot validate --config /etc/telebot/config.toml
telebot import-gramjs-session --from /path/to/TeleBox/config.json --to /var/lib/telebot/telegram.session
telebot check-session --config /etc/telebot/config.toml
telebot check-ai --config /etc/telebot/config.toml
telebot check-quote --config /etc/telebot/config.toml
telebot check-telegram-image --config /etc/telebot/config.toml --image /path/to/test-image.png
telebot serve --config /etc/telebot/config.toml
```

Commands that open the Telegram session must not run beside `telebot.service`. On a systemd host, use
`scripts/server/check-telegram.sh session|format|plugins|all` or
`scripts/server/check-telegram.sh image /path/to/test-image.png`; it stops the service, performs the
requested checks and verifies that the service becomes ready again.

Builds use the pinned Rust 1.90 toolchain, a committed `Cargo.lock` and a reusable container cache.
Deployment, health checks, rollback and upgrade notes are in `docs/operations.md`.
