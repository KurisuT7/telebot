#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: verify-package.sh ARCHIVE ARCHITECTURE" >&2
  exit 2
fi

archive=$1
architecture=$2
package_name="telebot-linux-$architecture"
temporary=$(mktemp -d)

cleanup() {
  rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

tar -tzf "$archive" > "$temporary/archive-files"
tar -xzf "$archive" -C "$temporary"
package_root="$temporary/$package_name"

for path in \
  telebot install.sh config.example.toml \
  deploy/telebot.env.example deploy/telebot.service \
  scripts/server/install.sh README.md README.zh-CN.md LICENSE CHANGELOG.md; do
  test -e "$package_root/$path"
done
test -x "$package_root/telebot"
test -x "$package_root/install.sh"
test -x "$package_root/scripts/server/install.sh"

version=$("$package_root/telebot" --version)
case "$version" in
  "telebot "*) ;;
  *)
    echo "unexpected version output: $version" >&2
    exit 1
    ;;
esac

if ldd "$package_root/telebot" | grep -q 'not found'; then
  echo "release binary has unresolved shared libraries" >&2
  exit 1
fi

if grep -E '/(target|src|\.git|\.github)/|Cargo\.(toml|lock)$' "$temporary/archive-files"; then
  echo "release archive contains development-only files" >&2
  exit 1
fi

printf 'package verified: %s (%s)\n' "$archive" "$version"
