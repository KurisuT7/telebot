#!/bin/sh
set -eu

repo_url="https://github.com/LyoSU/quote-api.git"
commit="6f91434c8d22fda57bb2d7ad452a9b45f2b35f21"
image="telebot-quote-api:6f91434c8d22"
source_dir="/opt/telebot/quote-api-source"
runtime_dir="/opt/telebot/quote-api"
compose_source="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)/deploy/quote-api.compose.yml"

command -v docker >/dev/null 2>&1 || {
  echo "docker is required" >&2
  exit 1
}
command -v git >/dev/null 2>&1 || {
  echo "git is required" >&2
  exit 1
}
test -f "$compose_source" || {
  echo "missing $compose_source" >&2
  exit 1
}

install -d -m 0755 /opt/telebot "$runtime_dir"
if [ ! -d "$source_dir/.git" ]; then
  git clone --filter=blob:none "$repo_url" "$source_dir"
fi
git -C "$source_dir" fetch --depth 1 origin "$commit"
git -C "$source_dir" checkout --detach "$commit"
test "$(git -C "$source_dir" rev-parse HEAD)" = "$commit"

if ! docker image inspect "$image" >/dev/null 2>&1; then
  docker build --pull --tag "$image" "$source_dir"
fi
install -m 0644 "$compose_source" "$runtime_dir/compose.yml"
docker compose -f "$runtime_dir/compose.yml" up -d --wait

curl -fsS http://127.0.0.1:3210/health >/dev/null
printf 'quote-api ready: image=%s commit=%s\n' "$image" "$commit"
