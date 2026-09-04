#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$repository_root"

: "${CARGO_TARGET_DIR:=target/search-attribution}"
export CARGO_TARGET_DIR

cargo test -p tinyvm-qjs --test string_search_attribution_controls -- --ignored --nocapture
