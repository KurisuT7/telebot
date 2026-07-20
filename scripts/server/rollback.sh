#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: rollback.sh RELEASE_ID" >&2
  exit 2
fi
release_id=$1
case "$release_id" in
  ''|*[!A-Za-z0-9._-]*) exit 2 ;;
esac
target="/opt/telebot/releases/$release_id"
old_release=$(readlink -f /opt/telebot/current)
test -x "$target/telebot"

ln -s "$target" /opt/telebot/current.next
mv -Tf /opt/telebot/current.next /opt/telebot/current
if ! systemctl restart telebot.service; then
  ln -s "$old_release" /opt/telebot/current.rollback
  mv -Tf /opt/telebot/current.rollback /opt/telebot/current
  systemctl restart telebot.service
  exit 1
fi
printf 'telebot rolled back to: %s\n' "$release_id"
