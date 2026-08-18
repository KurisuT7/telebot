# telebot

[简体中文](README.zh-CN.md) | English

`telebot` signs in with your Telegram account and responds to `.ai` and `.q` commands in chats. It
can answer questions, search the web, work with images, and turn messages into quote stickers or
images.

This is a Telegram user client, not a Bot API bot. It executes only commands sent by the signed-in
account and ignores commands from other users. This project is not affiliated with or endorsed by
Telegram. Use only an account you control and follow the
[Telegram API Terms of Service](https://core.telegram.org/api/terms).

## Features

- `.ai` provides offline answers, native web search, image input, and optional rolling context.
- `.q` renders one to five messages as a WebP sticker or PNG and can save the result to a sticker set.
- AI backends can use Gemini Interactions, OpenAI Chat Completions, or OpenAI Responses.
- Common AI settings, including the endpoint, model, key, prompt, and timeouts, can be changed from
  Saved Messages without rebuilding.
- Command concurrency is bounded, missed Telegram updates are caught up after short restarts, and a
  slow native-search request can start a parallel retry.

## Prerequisites

The release packages target 64-bit Linux with systemd and glibc 2.35 or newer on x86_64 or ARM64.
Installation requires:

- `curl`, `tar`, and `sha256sum`;
- your own Telegram `api_id` and `api_hash` from the
  [official Telegram setup](https://core.telegram.org/api/obtaining_api_id);
- an AI endpoint using Gemini Interactions, OpenAI Chat Completions, or OpenAI Responses, together
  with its model and API key.

Rust, Cargo, Git, and Docker are not required for the core installation.

## Install a release

Download the package for the current CPU and verify its checksum:

```sh
case "$(uname -m)" in
  x86_64) architecture=x86_64 ;;
  aarch64|arm64) architecture=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
curl -fLO "https://github.com/KurisuT7/telebot/releases/latest/download/telebot-linux-$architecture.tar.gz"
curl -fLO "https://github.com/KurisuT7/telebot/releases/latest/download/SHA256SUMS"
grep " telebot-linux-$architecture.tar.gz$" SHA256SUMS | sha256sum --check -
tar -xzf "telebot-linux-$architecture.tar.gz"
cd "telebot-linux-$architecture"
```

Create the service account, data directory, and configuration files:

```sh
sudo ./install.sh --prepare
sudoedit /etc/telebot/config.toml
sudoedit /etc/telebot/telebot.env
```

Set `telegram.api_id`, `ai.api_format`, `ai.base_url`, and `ai.model` in
`config.toml`. Set `TELEBOT_TELEGRAM_API_HASH` and the AI key variable named by
`ai.api_key_env` in `telebot.env`.

Install and start telebot:

```sh
sudo ./install.sh
```

The installer validates the configuration, asks for the Telegram phone number and login code, stores
the resulting session, installs the systemd service, and waits for `telebot is ready`. A 2FA
password is requested when needed; login codes and passwords are not echoed. Existing Telegram
accounts are supported, but account registration and QR login are not.

The example configuration leaves quote rendering disabled, so the path above does not require
Docker. To enable `.q` during installation, set `quote.enabled = true` and run:

```sh
sudo ./install.sh --with-quote
```

That optional path requires Docker, Docker Compose, Git, and curl. Source builds are documented in
[Contributing](CONTRIBUTING.md#development-checks). See the
[operations guide](docs/operations.md) for upgrades, checks, backups, and rollback.

## Telegram commands

### AI

- `.ai <question>` uses the current default mode; the example configuration searches by default.
- `.ai chat <question>` answers without web search.
- `.ai search <question>` forces native search when the selected API format supports it.
- Reply to text, a selected quote, a photo, or a static sticker and send `.ai` to use it as input.
- `.ai context on|off` or `.ai context <1-20>` controls rolling context for the current chat.
- `.ai reset` removes stored context for the current chat.
- `.ai status` and `.ai config` show effective settings without displaying the API key.
- `.ai help` shows the complete command guide in Telegram.

Answers retain headings, emphasis, code, lists, and links. Long answers are split at valid Telegram
UTF-16 entity boundaries so formatting and links remain aligned.

### Quote images and stickers

- Reply to a message with `.q [1-5]` to render one to five messages as a WebP sticker.
- `.q r [1-5]` includes quoted reply content.
- `.q image [1-5]` creates a PNG.
- `.q stories [1-5]` creates a Stories-sized PNG.
- `.q s` saves the replied image or sticker to the configured sticker set.
- `.q history` and `.q history <ID>` list or resend archived quotes.
- `.q config` shows sticker-set and archive settings; `history on|off` and
  `history limit <1-500>` manage the archive.

Quote archiving is off by default and begins with newly generated quotes when enabled. The oldest
items are removed as the configured item or byte limit is reached.

Quote images are rendered on your server and are not sent to a public rendering service. See the
[operations guide](docs/operations.md) for quote-api installation and updates.

The default command prefixes are `.`, `。`, `,`, `，`, `$`, `!`, and `！`; they are configurable.
Commands work in ordinary chats and Saved Messages. Telebot executes only commands sent by the
signed-in account.

## AI API formats

`ai.api_format` selects the request protocol. `ai.provider` is a display label. Telebot does not
guess a protocol from the provider, model, or URL, and the adapters contain no gateway-specific
branches.

| `api_format` | Native web search | Behavior |
| --- | --- | --- |
| `gemini_interactions` | Yes | Uses Gemini Interactions native search |
| `openai_responses` | Yes | Uses the Responses API `web_search` tool |
| `openai_chat_completions` | No | `.ai search` returns a non-search answer labelled as such |

For a generic Chat Completions endpoint, change `/etc/telebot/config.toml` as follows:

```toml
[ai]
provider = "generic-oai"
api_format = "openai_chat_completions"
api_key_env = "TELEBOT_AI_API_KEY"
base_url = "https://api.example.com/v1"
model = "example-model"
search_fallback_model = ""
default_search = false
```

Set `base_url` to the API prefix. Telebot appends `chat/completions` or `responses` when the URL does
not already end with the selected endpoint. Select `openai_responses` only when the service actually
implements the Responses API.

For native-search formats, if the first request has not completed within `search_hedge_seconds`,
telebot starts a second request and uses the first successful result. An empty `search_fallback_model`
keeps both requests on the primary model; setting it uses that model for the second request. Set
`search_hedge_seconds` to `0` to disable hedging. Text and image requests have separate
`search_timeout_seconds` and `image_search_timeout_seconds` budgets.

## Changing AI settings at runtime

The following commands are accepted only in Saved Messages and take effect immediately:

- `.ai config provider <api_format> <name> <base_url>`
- `.ai config model <primary> [search_fallback|off]`
- `.ai config key <key>` / `.ai config clear-key`
- `.ai config prompt <system_prompt>`
- `.ai config thinking <minimal|low|medium|high>`
- `.ai config search <on|off>`
- `.ai config timeout <text> <image> <hedge> <fallback>`
- `.ai config tokens <1-65536>`
- `.ai config collapse <on|off>`
- `.ai config message searching|thinking <text>`
- `.ai config reload` / `.ai config reset`

Runtime values are stored in `/var/lib/telebot/telebot.db`. `.ai config key` first hides the command
that contains the key and never repeats the value in a reply; `clear-key` returns to the server
environment variable. AI concurrency and plugin enablement still require a TOML change and restart.

## Data flow

- Telegram receives the message reads, edits, sends, and sticker-set operations performed by the client.
- During login, the phone number, code, and optional 2FA password are sent only to Telegram. Telebot
  stores the resulting session, not the code or password.
- The AI service receives the current question, replied text or image, and enabled rolling context.
- Quote rendering stays on the local quote-api service by default.
- Telegram authorization, runtime AI settings, context, and quote archives are stored under
  `/var/lib/telebot`.

Restrict `/etc/telebot/telebot.env` and `/var/lib/telebot` to the server administrator and the telebot
service account.

## Current limitations

- Release packages and the supplied operations scripts target 64-bit Linux with systemd and glibc
  2.35 or newer on x86_64 and ARM64.
- Login supports a phone number, login code, and 2FA password, but not QR login or account registration.
- Chat Completions has no standard native-search protocol.
- AI image input is limited to photos and static stickers; optional quote rendering requires
  quote-api and Docker.

## Project documentation

- [Operations and upgrades](docs/operations.md)
- [Contributing](CONTRIBUTING.md)
- [Security reports](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [MIT License](LICENSE)
