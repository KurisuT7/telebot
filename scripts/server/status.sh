#!/bin/sh
set -eu

systemctl --no-pager --full status telebot.service || true
echo
systemctl show telebot.service \
  -p ActiveState -p SubState -p MainPID -p NRestarts -p MemoryCurrent -p ExecMainStatus
echo
journalctl -u telebot.service -n 80 --no-pager

echo
if curl -fsS --max-time 5 http://127.0.0.1:3210/health >/dev/null 2>&1; then
  echo 'quote renderer health: ok'
elif command -v docker >/dev/null 2>&1; then
  docker ps --filter name=telebot-quote-api \
    --format 'quote renderer: {{.Names}} | {{.Status}} | {{.Ports}}' || true
  echo 'quote renderer health: unavailable'
else
  echo 'quote renderer: not installed (optional)'
fi
