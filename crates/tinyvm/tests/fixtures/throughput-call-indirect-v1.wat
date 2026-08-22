;; Interpreter throughput workload: indirect calls.
;; Adds table bounds, element liveness and a run-time type check to the same
;; activation path the direct-call row measures.
(module
  (type $unary (func (param i32) (result i32)))
  (table 2 funcref)
  (elem (i32.const 0) $inc $dec)
  (func $inc (type $unary) (i32.add (local.get 0) (i32.const 3)))
  (func $dec (type $unary) (i32.sub (local.get 0) (i32.const 1)))
  (func (export "run") (result i32)
    (local $i i32) (local $acc i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $acc (call_indirect (type $unary)
          (local.get $acc) (i32.and (local.get $i) (i32.const 1))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (local.get $acc)))
