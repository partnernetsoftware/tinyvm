;; Interpreter throughput workload: the dispatch floor.
;; The loop body is ordinary i32 ALU work, so whatever fixed cost the
;; interpreter pays per instruction shows up here undiluted.
(module
  (func (export "run") (result i32)
    (local $i i32) (local $acc i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $acc (i32.add (local.get $acc) (i32.mul (local.get $i) (i32.const 3))))
        (local.set $acc (i32.xor (local.get $acc) (i32.shr_u (local.get $acc) (i32.const 7))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (local.get $acc)))
