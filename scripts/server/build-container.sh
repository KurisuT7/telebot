#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
builder_image=${TELEBOT_BUILDER_IMAGE:-telebot-builder:rust-1.90}
cache_root=${TELEBOT_BUILD_CACHE_ROOT:-/var/cache/telebot-build}

case "$cache_root" in
  /*) ;;
  *)
    echo "TELEBOT_BUILD_CACHE_ROOT must be an absolute path" >&2
    exit 2
    ;;
esac

install -d -m 0755 "$cache_root/cargo" "$cache_root/target"
if ! docker image inspect "$builder_image" >/dev/null 2>&1; then
  docker build -f "$repo_root/deploy/Dockerfile.builder" -t "$builder_image" "$repo_root"
fi

docker run --rm \
  -e CARGO_HOME=/cargo \
  -v "$repo_root:/workspace" \
  -v "$cache_root/cargo:/cargo" \
  -v "$cache_root/target:/workspace/target" \
  -w /workspace \
  "$builder_image" \
  sh -c './scripts/server/build.sh'

install -d -m 0755 "$repo_root/target/release"
install -m 0755 "$cache_root/target/release/telebot" "$repo_root/target/release/telebot"
sha256sum "$repo_root/target/release/telebot"
