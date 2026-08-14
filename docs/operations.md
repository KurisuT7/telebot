# Operations

## Runtime layout

- Releases: `/opt/telebot/releases/<release-id>/`
- Active release: `/opt/telebot/current`
- Configuration: `/etc/telebot/config.toml`
- Secrets: `/etc/telebot/telebot.env`
- Session and state: `/var/lib/telebot/`
- Deployment backups: `/var/backups/telebot/<release-id>/`
- Service: `telebot.service`

Never commit the production configuration, environment file, Telegram session, database, logs, or release binaries.

## First-time setup

1. Copy `config.example.toml` to `/etc/telebot/config.toml` and set a Telegram API ID.
2. Copy `deploy/telebot.env.example` to `/etc/telebot/telebot.env` and replace the placeholders.
3. Restrict the environment file to root with mode `0600`.
4. Create the `telebot` system user and `/var/lib/telebot` with owner `telebot:telebot` and mode `0700`.
5. Import or authorize a Telegram session while `telebot.service` is stopped.

## Build and deploy

```sh
scripts/server/build-container.sh
sudo scripts/server/deploy.sh <release-id>
```

The container build pins Rust 1.90 and retains Cargo and target caches under
`/var/cache/telebot-build` so later source-only updates are incremental while still running formatting,
tests, Clippy and the release build. The deployment validates configuration before an atomic symlink
switch. If restart or readiness checks fail, it restores the previous release.

## Health and maintenance checks

```sh
scripts/server/status.sh
sudo scripts/server/check-telegram.sh session
sudo scripts/server/check-telegram.sh format
sudo scripts/server/check-telegram.sh plugins
sudo scripts/server/check-telegram.sh image /path/to/test-image.png
```

Telegram checks stop the service before opening its MTProto session and start it again afterward. Do not run the underlying Telegram check commands directly while the service is active.
The image check uploads the supplied local file to Saved Messages, replies with a native-search AI command,
verifies a Gemini answer, and deletes the temporary Telegram messages.

## Runtime AI configuration

Use `.ai help` for the complete command guide. Mutable AI settings are accepted only in Saved
Messages, validated, stored in SQLite and applied immediately. `.ai config reload` rereads the
`[ai]` and `[messages]` TOML defaults while preserving SQLite overrides; `.ai config reset` removes
all `ai.runtime.*` overrides. AI concurrency and plugin enable/disable still require a service
restart because they shape process resources and router registration.

## Rollback

```sh
sudo scripts/server/rollback.sh <existing-release-id>
```

Rollback only switches to an existing release; it never deletes state or releases.

## Persistent state and backups

The state database may contain an explicitly configured AI key, rolling AI context and archived
quote media. Keep `/var/lib/telebot` and its backups owner-only. Quote history is bounded by both
`history_limit` and `history_max_bytes`; the oldest records are removed automatically.

Use SQLite online backup (or stop the service before copying the database and WAL files), then run
`PRAGMA integrity_check` against the backup before relying on it for rollback.

## Quote renderer

The quote renderer is pinned to a reviewed upstream commit and listens only on `127.0.0.1:3210`.

```sh
sudo scripts/server/install-quote-api.sh
```
