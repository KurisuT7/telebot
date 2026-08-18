#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: package.sh TARGET ARCHITECTURE" >&2
  exit 2
fi

target=$1
architecture=$2
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
binary="$repo_root/target/$target/release/telebot"
archive_name="telebot-linux-$architecture"
dist="$repo_root/dist"
temporary=$(mktemp -d)

cleanup() {
  rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

test -x "$binary"
version=$("$binary" --version)
case "$version" in
  "telebot "*) ;;
  *)
    echo "unexpected version output: $version" >&2
    exit 1
    ;;
esac

package_root="$temporary/$archive_name"
install -d -m 0755 \
  "$package_root/deploy" \
  "$package_root/docs" \
  "$package_root/scripts/server"
install -m 0755 "$binary" "$package_root/telebot"
install -m 0755 "$repo_root/install.sh" "$package_root/install.sh"
for script in check-telegram.sh deploy.sh install-quote-api.sh install.sh login.sh rollback.sh status.sh; do
  install -m 0755 "$repo_root/scripts/server/$script" "$package_root/scripts/server/$script"
done
install -m 0644 "$repo_root/config.example.toml" "$package_root/config.example.toml"
install -m 0644 "$repo_root/deploy/telebot.env.example" "$package_root/deploy/telebot.env.example"
install -m 0644 "$repo_root/deploy/telebot.service" "$package_root/deploy/telebot.service"
install -m 0644 "$repo_root/deploy/quote-api.compose.yml" "$package_root/deploy/quote-api.compose.yml"
for document in README.md README.zh-CN.md LICENSE CHANGELOG.md; do
  install -m 0644 "$repo_root/$document" "$package_root/$document"
done
for document in \
  operations.md operations.zh-CN.md \
  gramjs-migration.md gramjs-migration.zh-CN.md; do
  install -m 0644 "$repo_root/docs/$document" "$package_root/docs/$document"
done

install -d -m 0755 "$dist"
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" show -s --format=%ct HEAD)}
TZ=UTC tar \
  --sort=name \
  --mtime="@$epoch" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  -C "$temporary" \
  -czf "$dist/$archive_name.tar.gz" \
  "$archive_name"
sha256sum "$dist/$archive_name.tar.gz"
