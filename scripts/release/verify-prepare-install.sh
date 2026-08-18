#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: verify-prepare-install.sh ARCHIVE ARCHITECTURE" >&2
  exit 2
fi

archive=$1
architecture=$2
package_name="telebot-linux-$architecture"
temporary=$(mktemp -d)
created_user=0

for path in /etc/telebot /opt/telebot /var/lib/telebot /var/backups/telebot; do
  if [ -e "$path" ]; then
    echo "refusing prepare-install test because $path already exists" >&2
    exit 1
  fi
done
if id telebot >/dev/null 2>&1 || getent group telebot >/dev/null 2>&1; then
  echo "refusing prepare-install test because the telebot account already exists" >&2
  exit 1
fi

cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$created_user" -eq 1 ]; then
    sudo rm -rf -- /etc/telebot /opt/telebot /var/lib/telebot /var/backups/telebot
    sudo userdel telebot >/dev/null 2>&1 || true
    sudo groupdel telebot >/dev/null 2>&1 || true
  fi
  rm -rf -- "$temporary"
  exit "$status"
}
trap cleanup EXIT HUP INT TERM

tar -xzf "$archive" -C "$temporary"
created_user=1
sudo "$temporary/$package_name/install.sh" --prepare

id telebot >/dev/null
test "$(stat -c '%a' /etc/telebot/config.toml)" = "640"
test "$(stat -c '%a' /etc/telebot/telebot.env)" = "600"
test "$(stat -c '%a' /var/lib/telebot)" = "700"
test "$(stat -c '%U:%G' /var/lib/telebot)" = "telebot:telebot"
grep -q '^api_id = 0$' /etc/telebot/config.toml
grep -q '^TELEBOT_TELEGRAM_API_HASH=replace_me$' /etc/telebot/telebot.env
printf 'prepare install verified: %s\n' "$package_name"
