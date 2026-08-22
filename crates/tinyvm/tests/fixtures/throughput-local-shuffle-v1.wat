;; Interpreter throughput workload: locals traffic in a wide frame.
;; local.get / local.set indexing is the whole body, so a frame-layout change
;; shows up here before anywhere else.
(module
  (func (export "run") (result i32)
    (local $i i32) (local $a i32) (local $b i32) (local $c i32)
    (local $d i32) (local $e i32) (local $f i32) (local $g i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $g (local.get $f))
        (local.set $f (local.get $e))
        (local.set $e (local.get $d))
        (local.set $d (local.get $c))
        (local.set $c (local.get $b))
        (local.set $b (local.get $a))
        (local.set $a (i32.add (local.get $g) (local.get $i)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (local.get $a)))
