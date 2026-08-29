#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: build-c-cartridge.sh SOURCE.c OUTPUT.wasm" >&2
  exit 2
fi

crate_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
source=$1
output=$2
if [ ! -f "$source" ]; then
  echo "cartridge source is not a regular file: $source" >&2
  exit 2
fi

clang_bin=${CLANG:-$(command -v clang || true)}
if [ -x /opt/homebrew/opt/llvm/bin/clang ]; then
  clang_bin=${CLANG:-/opt/homebrew/opt/llvm/bin/clang}
fi
# A clang can have the wasm32 backend and no wasm linker (Apple's, and a
# Homebrew llvm without lld): the link step then dies with "posix_spawn
# failed". zig ships lld, and `zig cc` takes clang's flags, spelling the
# bare-wasm triple `wasm32-freestanding`. An explicit CLANG always wins.
target=wasm32-unknown-unknown
if [ -z "${CLANG:-}" ] && command -v zig >/dev/null 2>&1; then
  has_wasm_ld=0
  if command -v wasm-ld >/dev/null 2>&1; then has_wasm_ld=1; fi
  if [ -n "$clang_bin" ] && [ -x "$(dirname -- "$clang_bin")/wasm-ld" ]; then has_wasm_ld=1; fi
  if [ "$has_wasm_ld" = 0 ]; then
    clang_bin="zig cc"
    target=wasm32-freestanding
  fi
fi
if [ -z "$clang_bin" ]; then
  echo "a clang with the wasm32 backend and a wasm linker is required (or zig, or set CLANG)" >&2
  exit 1
fi

mkdir -p "$(dirname -- "$output")"
# shellcheck disable=SC2086  # "zig cc" is two words on purpose
$clang_bin \
  --target="$target" \
  -std=c17 \
  -Oz \
  -ffreestanding \
  -fno-builtin \
  -nostdlib \
  -I "$crate_dir/include" \
  -Wl,--no-entry \
  -Wl,--export-memory \
  -Wl,--initial-memory=65536 \
  -Wl,--max-memory=65536 \
  -Wl,-z,stack-size=4096 \
  -Wl,--strip-all \
  "$source" \
  -o "$output"

printf '%s\n' "$output"
