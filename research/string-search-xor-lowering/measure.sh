#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$repository_root"

: "${CARGO_TARGET_DIR:=target/search-xor-lowering}"
export CARGO_TARGET_DIR

cargo test -p tinyvm-qjs --test index_of_cost
cargo test -p tinyvm-qjs \
  --test string_search_attribution_controls \
  build_only_and_historical_controls_close -- --ignored --nocapture
cargo test -p tinyvm-qjs \
  --lib string_search_attribution::cumulative_search_layers_are_attributed \
  -- --ignored --nocapture
cargo test -p tinyvm-qjs \
  --lib string_search_attribution::direct_xor_is_measured_against_the_arithmetic_baseline \
  -- --ignored --nocapture
