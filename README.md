# telebot

`telebot` is an independent Rust Telegram user client. It is intentionally small: the runtime,
command router, state store, AI provider and quote feature are separate modules, so new commands
can be added without modifying Telegram connection code.

Initial commands:

- `.ai <question>`: use native web search by default.
- Reply to text, a selected quote, a photo or a static sticker with `.ai` to use it for this request.
- `.ai chat <question>` / `.ai search <question>`: force offline or native-search mode.
- `.ai config`: inspect the active Gemini-compatible provider without revealing its key.
- `.ai config provider <name> <base_url>`: change the provider label and Gemini-compatible endpoint.
- `.ai config key <key>` / `.ai config model <primary> [search_fallback]`: update credentials or models.
- `.ai context <0-20|on|off>`: configure per-chat rolling context; it remains off by default.
- `.ai reset|status|help`: clear the current chat context, inspect configuration, or show help.
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

Default web answers continue to use Gemini native Google Search through the Interactions API. The
active provider can be replaced at runtime with a Gemini-compatible endpoint while keeping the same
grounding path. Stable primary and fallback models are hedged, and Google citation annotations are
deduplicated and rendered as Telegram links; no third-party search-result scraping is used.

## Design

- `src/main.rs`: process lifecycle, Telegram update stream and bounded command workers.
- `src/plugin.rs`: small plugin trait and command router; future features do not touch the connection loop.
- `src/plugins/ai.rs`: dynamic Gemini-compatible runtime settings, bounded context and native-search fallback.
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
telebot serve --config /etc/telebot/config.toml
```

Commands that open the Telegram session must not run beside `telebot.service`. On a systemd host, use
`scripts/server/check-telegram.sh session|format|plugins|all`; it stops the service, performs the
requested checks and verifies that the service becomes ready again.

Builds use the pinned Rust 1.90 toolchain and a committed `Cargo.lock`. Deployment, health checks,
rollback and upgrade notes are in `docs/operations.md`.
