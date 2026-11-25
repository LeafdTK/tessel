#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
command -v dx >/dev/null 2>&1 || { echo "needs dx: https://github.com/DioxusLabs/dioxus/releases (dx-aarch64-apple-darwin) into ~/.cargo/bin" >&2; exit 1; }
cd "$root"
exec dx serve --hot-patch -p tessel --bin tessel --features hot --platform macos "$@"
