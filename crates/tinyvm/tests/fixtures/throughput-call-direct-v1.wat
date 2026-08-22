;; Interpreter throughput workload: direct calls.
;; Activation setup, argument moves and the return path, measured against a
;; callee body small enough that the call itself dominates.
(module
  (func $leaf (param i32) (result i32)
    (i32.add (local.get 0) (i32.const 1)))
  (func (export "run") (result i32)
    (local $i i32) (local $acc i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $acc (call $leaf (local.get $acc)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (local.get $acc)))
