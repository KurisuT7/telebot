# telebot operations

[简体中文](operations.zh-CN.md) | English

This guide covers the repository's Linux, systemd, and Docker deployment path. Run commands from the
repository root unless noted otherwise. Commands that require root privileges use `sudo`.

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

### 2. Build

The host needs Docker and permission to use it. The build script pins Rust 1.90 and runs formatting,
tests, Clippy, and a release build:

```sh
sudo scripts/server/build-container.sh
```

Cargo downloads and build output are cached under `/var/cache/telebot-build`. The resulting binary is
copied to `target/release/telebot`, and the command prints its SHA-256 digest.

### 3. Create the service account and configuration

```sh
sudo useradd --system --home-dir /var/lib/telebot --shell "$(command -v nologin)" telebot
sudo install -d -m 0755 /etc/telebot
sudo install -d -o telebot -g telebot -m 0700 /var/lib/telebot
sudo install -m 0644 config.example.toml /etc/telebot/config.toml
sudo install -m 0600 deploy/telebot.env.example /etc/telebot/telebot.env
```

Skip `useradd` when the `telebot` account already exists.

At minimum, change these values in `/etc/telebot/config.toml`:

- `telegram.api_id`;
- `ai.api_format`, `ai.base_url`, `ai.model`, and `ai.search_fallback_model`;
- `ai.api_key_env` when the Gemini example variable is not appropriate.

Set the following in `/etc/telebot/telebot.env`:

- `TELEBOT_TELEGRAM_API_HASH`;
- the AI key variable named by `ai.api_key_env`.

The root-owned environment file remains mode `0600`. systemd reads it before starting telebot, so API
keys do not need to appear in TOML.

### 4. Log in to Telegram

Run the login wrapper:

```sh
sudo scripts/server/login.sh
```

Enter the phone number in international format and the Telegram login code. When two-step
verification is enabled, the wrapper also asks for the 2FA password. Neither the code nor the password
is echoed. The resulting session is stored at `/var/lib/telebot/telegram.session` with mode `0600`.

If `telebot.service` is active, the wrapper stops it before login and starts it again afterward. Do
not run the underlying login command beside the service. An already authorized session is reported
without sending another login code.

Existing GramJS sessions can be imported by following the separate
[migration guide](gramjs-migration.md). That path is not part of a new installation.

### 5. Install the quote renderer

```sh
sudo scripts/server/install-quote-api.sh
```

The script fetches the pinned `LyoSU/quote-api` commit recorded in the repository, builds a local
image, and starts the `telebot-quote-api` container. The service binds only to `127.0.0.1:3210`, has no
persistent volume, and includes CJK fonts in the image. Installation finishes with a `/health` check.

### 6. Deploy and verify

The release ID may contain only letters, numbers, dots, underscores, and hyphens:

```sh
sudo scripts/server/deploy.sh first
sudo systemctl enable telebot.service
sudo scripts/server/status.sh
```

Deployment:

1. copies the binary and operations files into a new release directory;
2. loads the environment and runs `telebot validate`;
3. atomically switches `/opt/telebot/current`;
4. restarts the service and waits for `telebot is ready` in the journal;
5. restores the previous release if the checks fail.

`status.sh` should report `ActiveState=active`, `SubState=running`, and quote renderer health `ok`.

## Updates

After updating the source, rebuild and deploy with a new release ID:

```sh
sudo scripts/server/build-container.sh
sudo scripts/server/deploy.sh 2026-08-17
sudo scripts/server/status.sh
```

Do not reuse an existing release ID. Deployment does not remove older releases, configuration,
sessions, or SQLite state.

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
