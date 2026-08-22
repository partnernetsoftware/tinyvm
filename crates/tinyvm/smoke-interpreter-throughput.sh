#!/bin/sh
# In-guest interpreter throughput, with WABT as the semantic oracle.
#
# Timing alone proves nothing: an engine that computes the wrong answer is
# always fast. So every fixture is compiled with wat2wasm, validated, and run
# through WABT's independent interpreter first. Only bytes WABT has already
# agreed with are handed to the timed tinyvm run, and tinyvm must return the
# same value again through its own host oracle.
#
# Timings are development evidence, not release thresholds. The gate is the
# matrix: eight workload shapes, one positive observation each, same answers.
set -eu

ROOT=$(unset CDPATH; cd -- "$(dirname -- "$0")/../.." && pwd)
CRATE="$ROOT/crates/tinyvm"
TEMP=$(mktemp -d)
trap 'rm -rf -- "$TEMP"' EXIT HUP INT TERM
CARGO=${CARGO:-cargo}
WAT2WASM=${WAT2WASM:-wat2wasm}
WASM_VALIDATE=${WASM_VALIDATE:-wasm-validate}
WASM_INTERP=${WASM_INTERP:-wasm-interp}

WORKLOADS='throughput-i32-loop-v1
throughput-i64-loop-v1
throughput-f64-math-v1
throughput-memory-scan-v1
throughput-call-direct-v1
throughput-call-indirect-v1
throughput-br-table-v1
throughput-local-shuffle-v1'

count=0
for name in $WORKLOADS; do
  wasm="$TEMP/$name.wasm"
  "$WAT2WASM" "$CRATE/tests/fixtures/$name.wat" -o "$wasm"
  "$WASM_VALIDATE" "$wasm"
  interp=$("$WASM_INTERP" "$wasm" --run-all-exports)
  case "$interp" in
    'run() => i32:'*) ;;
    *)
      echo "FAIL: $name did not return one i32 through WABT: $interp" >&2
      exit 1
      ;;
  esac
  printf '%s,%s\n' "$name" "${interp#run\(\) => i32:}" >>"$TEMP/wabt-answers.txt"
  count=$((count + 1))
done
if [ "$count" -ne 8 ]; then
  echo "FAIL: expected eight workload shapes, compiled $count" >&2
  exit 1
fi

# Release build: a debug interpreter measures the debug build, not the product.
TINYVM_OUTPUT=$(TINYVM_THROUGHPUT_WASM_DIR="$TEMP" \
  "$CARGO" test --release -q -p tinyvm \
  --test interpreter_throughput \
  interpreter_throughput_reports_nanoseconds_per_guest_instruction -- \
  --ignored --exact --nocapture)
printf '%s\n' "$TINYVM_OUTPUT"

# tinyvm agreeing with the host oracle is asserted inside the test; here we
# require that it published a complete, positive matrix.
printf '%s\n' "$TINYVM_OUTPUT" | awk -F, '
  $1 == "tinyvm" {
    if (NF != 7 || $4 <= 0 || $5 <= 0 || $6 <= 0 || $7 <= 0) exit 1
    count++
  }
  END { if (count != 8) exit 1 }
'

echo "OK: WABT and tinyvm agree on eight workload shapes; tinyvm published ns/guest-instruction for each"
