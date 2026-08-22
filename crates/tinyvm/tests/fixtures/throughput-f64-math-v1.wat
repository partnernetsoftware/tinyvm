;; Interpreter throughput workload: float lanes.
;; A no_std build routes some of these through libm, so keeping a float row
;; stops a scalar-only optimization from silently regressing them.
(module
  (func (export "run") (result i32)
    (local $i i32) (local $acc f64)
    (local.set $acc (f64.const 1))
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $acc (f64.add (f64.mul (local.get $acc) (f64.const 1.0000001)) (f64.const 0.5)))
        (local.set $acc (f64.sub (local.get $acc) (f64.floor (f64.mul (local.get $acc) (f64.const 0.5)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (i32.trunc_sat_f64_s (local.get $acc))))
