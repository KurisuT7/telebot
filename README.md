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
  slow native-search request can start a fallback model.

## Prerequisites

The supplied deployment scripts target a Linux server with systemd. A first deployment requires:

- Docker, Docker Compose, Git, and curl;
- your own Telegram `api_id` and `api_hash` from the
  [official Telegram setup](https://core.telegram.org/api/obtaining_api_id);
- an AI endpoint using Gemini Interactions, OpenAI Chat Completions, or OpenAI Responses, together
  with its model and API key.

Run the commands below from the repository root.

## First deployment

Build telebot:

```sh
sudo scripts/server/build-container.sh
```

Create the service account and directories, then install the example configuration:

```sh
sudo useradd --system --home-dir /var/lib/telebot --shell "$(command -v nologin)" telebot
sudo install -d -m 0755 /etc/telebot
sudo install -d -o telebot -g telebot -m 0700 /var/lib/telebot
sudo install -m 0644 config.example.toml /etc/telebot/config.toml
sudo install -m 0600 deploy/telebot.env.example /etc/telebot/telebot.env
```

Set `telegram.api_id` and the AI endpoint in `/etc/telebot/config.toml`. Set
`TELEBOT_TELEGRAM_API_HASH` and the variable named by `ai.api_key_env` in
`/etc/telebot/telebot.env`. Do not commit these files, a Telegram session, or the state database.

Authorize the Telegram account:

```sh
sudo scripts/server/login.sh
```

Enter the phone number in international format and the Telegram login code. If the account uses
two-step verification, telebot also asks for its password. The code and password are not echoed to
the terminal. Login works with an existing Telegram account; it cannot register a new one.

Install the local quote renderer and deploy telebot:

```sh
sudo scripts/server/install-quote-api.sh
sudo scripts/server/deploy.sh first
sudo systemctl enable telebot.service
sudo scripts/server/status.sh
```

Deployment validates the configuration before switching releases. If startup fails or the process
does not log `telebot is ready` within 35 seconds, the script restores the previous release. See the
[operations guide](docs/operations.md) for updates, checks, backups, and rollback.

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
search_fallback_model = "example-model"
default_search = false
```

Set `base_url` to the API prefix. Telebot appends `chat/completions` or `responses` when the URL does
not already end with the selected endpoint. Select `openai_responses` only when the service actually
implements the Responses API.

For native-search formats, if the primary model has not completed within `search_hedge_seconds`,
telebot starts `search_fallback_model` and uses the first successful result. Text and image requests
have separate `search_timeout_seconds` and `image_search_timeout_seconds` budgets.

## Changing AI settings at runtime

The following commands are accepted only in Saved Messages and take effect immediately:

- `.ai config provider <api_format> <name> <base_url>`
- `.ai config model <primary> [search_fallback]`
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

- The supplied deployment and operations scripts target Linux, systemd, and Docker.
- Login supports a phone number, login code, and 2FA password, but not QR login or account registration.
- Chat Completions has no standard native-search protocol.
- AI image input is limited to photos and static stickers; quote rendering requires quote-api.

## Project documentation

- [Operations and upgrades](docs/operations.md)
- [Contributing](CONTRIBUTING.md)
- [Security reports](SECURITY.md)
- [Changelog](CHANGELOG.md)
- [MIT License](LICENSE)
