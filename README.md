# telebot

`telebot` is an independent Rust Telegram user client. It is intentionally small: the runtime,
command router, state store, AI provider and quote feature are separate modules, so new commands
can be added without modifying Telegram connection code.

Initial commands:

- `.ai <question>`: Gemini with web search enabled by default; no conversation history is stored.
- Reply to text, a selected quote, a photo or a static sticker with `.ai` to use it for this request.
- `.ai chat <question>`: explicitly answer without web search.
- `.ai search <question>`: explicitly answer with web search.
- `.ai reset|status|help`: clear legacy context, inspect configuration, or show help.
- `.q [1-5]`: generate a quote sticker from the replied message and following messages.
- `.q r [1-5]`: include reply blocks in the generated quote.
- `.q image [1-5]`: generate a PNG image instead of a sticker.
- `.q stories [1-5]`: generate a story-sized PNG image.
- `.q s`: add a replied sticker or photo to the configured sticker set.
- `.q config [sticker <short_name>]`: inspect or update quote configuration.

All commands accept any configured prefix. The production default is `.`, `。`, `,`, `，`, `$`,
`!`, and `！`.

Telegram may synchronize commands written by the same account with `outgoing=false`, especially
in Saved Messages. The router accepts a command only when it is outgoing, its sender is the
authorized account, or its peer is Saved Messages. Incoming commands from other users are ignored.
Production enables update catch-up so commands written during a short restart are not lost.

AI answers use native Telegram rich-text entities. Markdown headings, emphasis, code, lists and
links are preserved; long answers are placed in an expandable block quote and oversized answers
are split without breaking UTF-16 entity offsets. The service stores Telegram session state and
application settings in separate SQLite databases. Secrets are read from environment variables
and are never written to the application database.

Default web answers use Gemini's native Google Search through the Interactions API. The primary
model is pinned to stable `gemini-3.5-flash` with minimal thinking for predictable latency. If it
has not completed after 10 seconds, a stable `gemini-3.1-flash-lite` native-search request is
started and the first valid grounded answer wins. Google-provided citation annotations are
deduplicated and rendered as Telegram links; no third-party search-result scraping is used.

## Design

- `src/main.rs`: process lifecycle, Telegram update stream and bounded command workers.
- `src/plugin.rs`: small plugin trait and command router; future features do not touch the connection loop.
- `src/plugins/ai.rs`: stateless Gemini requests, reply/image context, default search and bounded fallback.
- `src/plugins/quote.rs`: local quote renderer payloads, media limits and Telegram sticker-set operations.
- `src/telegram.rs`: Telegram-native Markdown formatting, expandable questions and answers and safe message splitting.
- `src/store.rs`: one asynchronous SQLite engine shared with the Telegram session stack.
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
