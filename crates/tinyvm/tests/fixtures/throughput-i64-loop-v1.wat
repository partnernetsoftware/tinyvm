;; Interpreter throughput workload: the same shape in 64-bit lanes.
;; Separates "the value representation is wide" from "the dispatch is slow".
(module
  (func (export "run") (result i32)
    (local $i i32) (local $acc i64)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $acc (i64.add (local.get $acc)
          (i64.mul (i64.extend_i32_s (local.get $i)) (i64.const 3))))
        (local.set $acc (i64.xor (local.get $acc) (i64.shr_u (local.get $acc) (i64.const 7))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (i32.wrap_i64 (local.get $acc))))
