# telebot operations

[简体中文](operations.zh-CN.md) | English

This guide covers release-package operation on 64-bit Linux with systemd and glibc 2.35 or newer.
Docker is used only by the optional quote renderer. Run commands from an extracted release directory
unless noted otherwise. Commands that require root privileges use `sudo`.

## Files and directories

| Content | Path |
| --- | --- |
| Deployed releases | `/opt/telebot/releases/<release-id>/` |
| Active release symlink | `/opt/telebot/current` |
| Main configuration | `/etc/telebot/config.toml` |
| Telegram and AI secrets | `/etc/telebot/telebot.env` |
| Telegram session, SQLite state, and quote archive | `/var/lib/telebot/` |
| Deployment rollback records | `/var/backups/telebot/<release-id>/` |
| systemd unit | `telebot.service` |

Do not commit production configuration, environment files, Telegram sessions, databases, logs, or
compiled binaries.

## First deployment

### 1. Prepare accounts and credentials

Create your own `api_id` and `api_hash` using the
[official Telegram instructions](https://core.telegram.org/api/obtaining_api_id). Also prepare:

- an AI API key;
- its model name, API URL, and API format;
- an existing Telegram account that can sign in normally.

Telebot can authorize an existing account with its phone number, login code, and 2FA password. It
cannot register a new account and does not currently support QR login.

### 2. Download and verify

Follow the architecture detection, release download, and SHA-256 verification commands in the
[root README](../README.md#install-a-release), then enter the extracted release directory. Release
packages are provided for x86_64 and ARM64 Linux; they do not require Rust or Cargo on the server.

### 3. Create the service account and configuration

```sh
sudo ./install.sh --prepare
sudoedit /etc/telebot/config.toml
sudoedit /etc/telebot/telebot.env
```

The prepare step creates the `telebot` service account, `/var/lib/telebot`, and both configuration
files without overwriting existing files. At minimum, set:

- `telegram.api_id`, `ai.api_format`, `ai.base_url`, and `ai.model` in `config.toml`;
- `TELEBOT_TELEGRAM_API_HASH` and the AI key variable named by `ai.api_key_env` in `telebot.env`.

The environment file is root-owned with mode `0600`. systemd reads it before starting telebot, so
keys do not need to appear in TOML.

### 4. Authorize, deploy, and verify

```sh
sudo ./install.sh
```

The installer validates the files, requests the Telegram phone number and login code, installs a
versioned release, enables the systemd service, and waits for `telebot is ready`. When two-step
verification is enabled, it also requests the 2FA password. Neither the code nor password is echoed.
The resulting session is stored at `/var/lib/telebot/telegram.session` with mode `0600`.

Deployment:

1. copies the binary and operations files into `/opt/telebot/releases/v<version>/`;
2. loads the environment and runs `telebot validate`;
3. atomically switches `/opt/telebot/current`;
4. restarts the service and waits for `telebot is ready`;
5. restores the previous release if the checks fail.

Existing GramJS sessions can be imported by following the separate
[migration guide](gramjs-migration.md). That path is not part of a new installation.

### 5. Optional quote renderer

The example configuration has `quote.enabled = false`. To enable `.q` during installation, change
that setting and run:

```sh
sudo ./install.sh --with-quote
```

This path requires Docker, Docker Compose, Git, and curl. It fetches the pinned
`LyoSU/quote-api` commit, builds a local image, starts a loopback-only service, and checks
`http://127.0.0.1:3210/health`.

## Updates

Download and verify the newer release as described in the README, extract it, and run:

```sh
cd "telebot-linux-$(uname -m)"
sudo ./install.sh
```

For ARM systems that report `arm64`, the extracted directory is `telebot-linux-aarch64`. Each
version uses a distinct release directory. Installation does not remove older releases,
configuration, Telegram authorization, or SQLite state.

## Routine checks

Inspect the service, recent journal entries, and quote-api health:

```sh
sudo scripts/server/status.sh
```

The following checks open the Telegram MTProto session and must not run beside `telebot.service`.
The wrapper stops the service, performs the selected check, starts the service again, and waits for it
to become ready:

```sh
sudo scripts/server/check-telegram.sh session
sudo scripts/server/check-telegram.sh format
sudo scripts/server/check-telegram.sh plugins
sudo scripts/server/check-telegram.sh image /path/to/test-image.png
```

- `session` confirms that the Telegram session remains authorized.
- `format` sends a Saved Messages test, asks Telegram to accept the rich-text entities, and deletes it.
- `plugins` performs live AI reply, help, runtime-message, and quote-sticker checks, then deletes the
  test messages.
- `image` uploads the supplied file to Saved Messages, performs a native-search image request, and
  deletes the test messages.

The last two checks make real AI API calls. Do not use an image containing personal information.

## AI configuration

`ai.api_format` accepts:

- `gemini_interactions`
- `openai_chat_completions`
- `openai_responses`

For an OpenAI-compatible endpoint, set `ai.base_url` to a versioned API prefix such as
`https://api.example.com/v1`. Telebot appends `chat/completions` or `responses` according to
`ai.api_format`; it does not infer a format from a name or URL.

Native web search is implemented for Gemini Interactions and OpenAI Responses. A Chat Completions
search command returns an ordinary answer with a clear note that no web search was performed.

Use `.ai config` in Saved Messages to change common settings. Values are stored in SQLite and override
TOML:

- `.ai config reload` rereads TOML while retaining the SQLite overrides.
- `.ai config reset` removes all `ai.runtime.*` overrides and returns to TOML.
- AI concurrency and plugin enablement are read only at startup and require a service restart.

## Backups

`/var/lib/telebot/telebot.db` may contain runtime settings, an explicitly stored AI key, chat context,
and archived quotes. The Telegram session is stored in the same directory. Protect backups with the
same access restrictions as the live files.

Use SQLite's online backup mechanism. If files are copied directly, stop telebot first and copy the
database together with any existing WAL files. Run `PRAGMA integrity_check` against the result before
relying on it for recovery.

`/etc/telebot` and `/var/lib/telebot` are outside the release directories. Switching or deleting an old
release does not restore configuration or state.

## Rollback

Choose an existing version from `/opt/telebot/releases/`:

```sh
sudo scripts/server/rollback.sh <existing-release-id>
sudo scripts/server/status.sh
```

Rollback switches the executable and restarts the service. It does not change configuration, state,
the Telegram session, or any release directory. If the selected version cannot start, the script
attempts to restore the version that was active before rollback.

## Stop the deployment

```sh
sudo systemctl disable --now telebot.service
sudo docker compose -f /opt/telebot/quote-api/compose.yml down
```

These commands retain `/etc/telebot`, `/var/lib/telebot`, release directories, build caches, and the
quote-api source checkout. The repository has no automatic uninstall command. Back up and verify
state before removing any of those paths manually.
