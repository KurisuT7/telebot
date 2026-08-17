#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
config=${TELEBOT_CONFIG:-/etc/telebot/config.toml}
environment=${TELEBOT_ENVIRONMENT:-/etc/telebot/telebot.env}
if [ -n "${TELEBOT_BINARY:-}" ]; then
  binary=$TELEBOT_BINARY
elif [ -x "$repo_root/target/release/telebot" ]; then
  binary="$repo_root/target/release/telebot"
else
  binary=/opt/telebot/current/telebot
fi

if [ ! -x "$binary" ]; then
  echo "telebot binary is missing or not executable: $binary" >&2
  exit 1
fi
if [ ! -r "$config" ]; then
  echo "telebot configuration is not readable: $config" >&2
  exit 1
fi
if [ ! -r "$environment" ]; then
  echo "telebot environment file is not readable: $environment" >&2
  exit 1
fi

restart=0
if systemctl is-active --quiet telebot.service; then
  restart=1
  systemctl stop telebot.service
fi

restore_service() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$restart" -eq 1 ]; then
    systemctl start telebot.service || exit 1
    ready=0
    for _second in $(seq 1 35); do
      service_pid=$(systemctl show telebot.service -p MainPID --value)
      if [ "$service_pid" -gt 0 ] \
        && journalctl _PID="$service_pid" --no-pager | grep -q 'telebot is ready'; then
        ready=1
        break
      fi
      sleep 1
    done
    if [ "$ready" -ne 1 ]; then
      echo "telebot did not recover after Telegram login" >&2
      exit 1
    fi
  fi
  exit "$status"
}
trap restore_service EXIT HUP INT TERM

set -a
. "$environment"
set +a
"$binary" login --config "$config"

chown -R telebot:telebot /var/lib/telebot
chmod 0700 /var/lib/telebot
find /var/lib/telebot -maxdepth 1 -type f -exec chmod 0600 {} +

echo "Telegram authorization is ready"
