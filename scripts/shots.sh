#!/bin/sh
set -e
out="${TESSEL_SHOTS:?TESSEL_SHOTS says where the pictures go}"
target="${CARGO_TARGET_DIR:-target}"
cargo build -q -p tessel --bin tessel
"$target/debug/tessel" --shots "$out"
