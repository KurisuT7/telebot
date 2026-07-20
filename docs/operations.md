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
scripts/server/build.sh
sudo scripts/server/deploy.sh <release-id>
```

The deployment validates configuration before an atomic symlink switch. If restart or readiness checks fail, it restores the previous release.

## Health and maintenance checks

```sh
scripts/server/status.sh
sudo scripts/server/check-telegram.sh session
sudo scripts/server/check-telegram.sh format
sudo scripts/server/check-telegram.sh plugins
```

Telegram checks stop the service before opening its MTProto session and start it again afterward. Do not run the underlying Telegram check commands directly while the service is active.

## Rollback

```sh
sudo scripts/server/rollback.sh <existing-release-id>
```

Rollback only switches to an existing release; it never deletes state or releases.

## Quote renderer

The quote renderer is pinned to a reviewed upstream commit and listens only on `127.0.0.1:3210`.

```sh
sudo scripts/server/install-quote-api.sh
```
