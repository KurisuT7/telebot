#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
config=/etc/telebot/config.toml
environment=/etc/telebot/telebot.env
prepare_only=0
with_quote=0

usage() {
  cat <<'EOF'
usage: install.sh [--prepare] [--with-quote]

  --prepare     create the service account, directories, and example configuration
  --with-quote  install the optional Docker-based quote renderer before starting
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prepare) prepare_only=1 ;;
    --with-quote) with_quote=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [ "$(id -u)" -ne 0 ]; then
  echo "install.sh must run as root" >&2
  exit 1
fi
command -v systemctl >/dev/null 2>&1 || {
  echo "systemd is required" >&2
  exit 1
}

if ! getent group telebot >/dev/null 2>&1; then
  groupadd --system telebot
fi
if ! id telebot >/dev/null 2>&1; then
  nologin=$(command -v nologin || true)
  if [ -z "$nologin" ]; then
    nologin=/usr/sbin/nologin
  fi
  useradd --system --gid telebot --home-dir /var/lib/telebot --shell "$nologin" telebot
fi

install -d -o root -g root -m 0755 /etc/telebot /opt/telebot /opt/telebot/releases
install -d -o root -g root -m 0700 /var/backups/telebot
install -d -o telebot -g telebot -m 0700 /var/lib/telebot

created=0
if [ ! -e "$config" ]; then
  install -o root -g telebot -m 0640 "$repo_root/config.example.toml" "$config"
  created=1
fi
if [ ! -e "$environment" ]; then
  install -o root -g root -m 0600 "$repo_root/deploy/telebot.env.example" "$environment"
  created=1
fi

if [ "$prepare_only" -eq 1 ] || [ "$created" -eq 1 ]; then
  printf '%s\n' "Configuration is ready:"
  printf '  %s\n' "$config" "$environment"
  printf '%s\n' "Fill both files, then run sudo ./install.sh"
  if [ "$with_quote" -eq 1 ]; then
    printf '%s\n' "After editing, run sudo ./install.sh --with-quote"
  fi
  exit 0
fi

if [ -n "${TELEBOT_BINARY:-}" ]; then
  binary=$TELEBOT_BINARY
elif [ -x "$repo_root/telebot" ]; then
  binary="$repo_root/telebot"
elif [ -x "$repo_root/target/release/telebot" ]; then
  binary="$repo_root/target/release/telebot"
else
  echo "telebot binary is missing from the release package or target/release" >&2
  exit 1
fi

version=$("$binary" --version)
case "$version" in
  "telebot "*) release_id="v${version#telebot }" ;;
  *)
    echo "unexpected version output: $version" >&2
    exit 1
    ;;
esac

quote_enabled=$(
  awk '
    /^\[quote\][[:space:]]*$/ { in_quote = 1; next }
    /^\[/ { in_quote = 0 }
    in_quote && /^[[:space:]]*enabled[[:space:]]*=/ {
      value = $0
      sub(/^[^=]*=[[:space:]]*/, "", value)
      sub(/[[:space:]#].*$/, "", value)
      print value
      exit
    }
  ' "$config"
)

if [ "$quote_enabled" = "true" ]; then
  if [ "$with_quote" -eq 1 ]; then
    "$repo_root/scripts/server/install-quote-api.sh"
  elif ! curl -fsS --max-time 5 http://127.0.0.1:3210/health >/dev/null 2>&1; then
    echo "quote.enabled=true, but quote-api is not ready" >&2
    echo "rerun with --with-quote, or set quote.enabled=false" >&2
    exit 1
  fi
fi

TELEBOT_BINARY="$binary" "$repo_root/scripts/server/login.sh"
release_dir="/opt/telebot/releases/$release_id"
if [ -e "$release_dir" ]; then
  current_release=$(readlink -f /opt/telebot/current 2>/dev/null || true)
  if [ "$current_release" != "$release_dir" ]; then
    echo "release directory already exists but is not active: $release_dir" >&2
    exit 1
  fi
else
  TELEBOT_BINARY="$binary" "$repo_root/scripts/server/deploy.sh" "$release_id"
fi
systemctl enable telebot.service
"$repo_root/scripts/server/status.sh"
