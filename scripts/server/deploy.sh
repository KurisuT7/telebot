#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
release_id=${1:-$(date -u +%Y%m%d-%H%M%S)}
case "$release_id" in
  ''|*[!A-Za-z0-9._-]*)
    echo "release id may only contain letters, digits, dot, underscore and hyphen" >&2
    exit 2
    ;;
esac

if [ -n "${TELEBOT_BINARY:-}" ]; then
  binary=$TELEBOT_BINARY
elif [ -x "$repo_root/telebot" ]; then
  binary="$repo_root/telebot"
else
  binary="$repo_root/target/release/telebot"
fi
release_dir="/opt/telebot/releases/$release_id"
backup_dir="/var/backups/telebot/$release_id"
config=/etc/telebot/config.toml
environment=/etc/telebot/telebot.env
unit="$repo_root/deploy/telebot.service"
old_release=$(readlink -f /opt/telebot/current 2>/dev/null || true)

test -x "$binary"
test -r "$config"
test -r "$environment"
test -f "$unit"
[ ! -e "$release_dir" ]

install -d -m 0755 "$release_dir"
install -d -m 0700 "$backup_dir"
install -o root -g root -m 0755 "$binary" "$release_dir/telebot"
cp -a "$repo_root/scripts" "$release_dir/scripts"
cp -a "$repo_root/docs" "$release_dir/docs"
cp -a "$repo_root/deploy" "$release_dir/deploy"
install -o root -g root -m 0644 "$repo_root/README.md" "$release_dir/README.md"
printf '%s\n' "$old_release" > "$backup_dir/previous-release"
cp -a /etc/systemd/system/telebot.service "$backup_dir/telebot.service" 2>/dev/null || true

set -a
. "$environment"
set +a
"$release_dir/telebot" validate --config "$config"

rollback() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$status" -ne 0 ] && [ -n "$old_release" ]; then
    ln -s "$old_release" /opt/telebot/current.rollback
    mv -Tf /opt/telebot/current.rollback /opt/telebot/current
    systemctl restart telebot.service || true
  fi
  exit "$status"
}
trap rollback EXIT HUP INT TERM

install -o root -g root -m 0644 "$unit" /etc/systemd/system/telebot.service
ln -s "$release_dir" /opt/telebot/current.next
mv -Tf /opt/telebot/current.next /opt/telebot/current
systemctl daemon-reload
systemctl restart telebot.service

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
[ "$ready" -eq 1 ]
trap - EXIT HUP INT TERM
printf 'telebot release active: %s\n' "$release_id"
