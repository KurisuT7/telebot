#!/bin/sh
set -eu

package_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "$package_root/scripts/server/install.sh" "$@"
