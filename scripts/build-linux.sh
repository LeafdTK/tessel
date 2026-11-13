#!/bin/sh
# tessel for a linux dev box: tesseld and tesselctl, static (musl), for
# x86_64 and arm64, cross-built here with zig. what a mac copies to a host
# when it sets it up (Settings › Hosts, or `tesselctl host add`).
#
#   scripts/build-linux.sh [x86_64|aarch64 ...]   ->  target/<arch>-unknown-linux-musl/release/
#
# needs cargo-zigbuild (brew install cargo-zigbuild) and zig 0.15 (ghostty's
# build wants exactly that one: brew's newer zig won't do).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
zig015=$(brew --prefix zig@0.15 2>/dev/null || true)
[ -n "$zig015" ] && [ -x "$zig015/bin/zig" ] && PATH="$zig015/bin:$PATH" && export PATH
command -v cargo-zigbuild >/dev/null 2>&1 || { echo "needs cargo-zigbuild: brew install cargo-zigbuild" >&2; exit 1; }
archs=${*:-x86_64 aarch64}
for arch in $archs; do
    rustup target add "$arch-unknown-linux-musl" >/dev/null 2>&1 || true
    (cd "$root" && cargo zigbuild --release --target "$arch-unknown-linux-musl" -p tesseld -p tesselctl)
    echo "$root/target/$arch-unknown-linux-musl/release"
done
