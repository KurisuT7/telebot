#!/bin/sh
set -eu

mode=${1:-all}
case "$mode" in
  session|format|plugins|all) ;;
  image)
    if [ "$#" -ne 2 ]; then
      echo "usage: check-telegram.sh image /path/to/test-image" >&2
      exit 2
    fi
    image=$2
    ;;
  *)
    echo "usage: check-telegram.sh [session|format|plugins|all|image /path/to/test-image]" >&2
    exit 2
    ;;
esac

binary=/opt/telebot/current/telebot
config=/etc/telebot/config.toml
environment=/etc/telebot/telebot.env
test -x "$binary"
test -r "$config"
test -r "$environment"

restart=0
if systemctl is-active --quiet telebot.service; then
  restart=1
  systemctl stop telebot.service
fi

restore_service() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$restart" -eq 1 ]; then
    systemctl start telebot.service
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
      echo "telebot did not recover after Telegram maintenance check" >&2
      exit 1
    fi
  fi
  exit "$status"
}
trap restore_service EXIT HUP INT TERM

set -a
. "$environment"
set +a

case "$mode" in
  session)
    "$binary" check-session --config "$config"
    ;;
  format)
    "$binary" check-telegram-format --config "$config"
    ;;
  plugins)
    "$binary" check-telegram-plugins --config "$config"
    ;;
  image)
    "$binary" check-telegram-image --config "$config" --image "$image"
    ;;
  all)
    "$binary" check-session --config "$config"
    "$binary" check-telegram-format --config "$config"
    "$binary" check-telegram-plugins --config "$config"
    ;;
esac

echo "Telegram maintenance check passed: $mode"
