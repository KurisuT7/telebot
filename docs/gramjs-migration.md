# Importing a GramJS session

[简体中文](gramjs-migration.zh-CN.md) | English

This guide is only for users who already have a GramJS StringSession and want to retain the current
Telegram authorization. New installations should run `scripts/server/login.sh` and do not need a
session JSON file.

Stop `telebot.service` and retain the original JSON before migration. The file must contain a string
field named `session`, and the destination session must not exist:

```sh
sudo ./target/release/telebot import-gramjs-session \
  --from /path/to/gramjs-session.json \
  --to /var/lib/telebot/telegram.session
sudo chown -R telebot:telebot /var/lib/telebot
sudo chmod 0700 /var/lib/telebot
sudo chmod 0600 /var/lib/telebot/telegram.session
```

The importer reads only the GramJS StringSession from the JSON. It does not modify the source or
overwrite an existing destination. After deployment, run
`sudo scripts/server/check-telegram.sh session`. Retain or remove the old configuration only after the
check passes and the service becomes ready again.

Both the source JSON and the imported session authorize Telegram access. Treat them as key
material; do not commit them or attach them to a public issue.
