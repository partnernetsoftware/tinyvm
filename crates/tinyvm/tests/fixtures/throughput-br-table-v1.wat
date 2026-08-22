;; Interpreter throughput workload: branch-heavy control flow.
;; The control stack, not the operand stack, is what gets pushed and popped
;; here. Falling out of $a runs the +1 arm and both trailing arms; $b runs +2
;; then +4; $c runs only +4.
(module
  (func (export "run") (result i32)
    (local $i i32) (local $acc i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (block $c
          (block $b
            (block $a
              (br_table $a $b $c (i32.rem_u (local.get $i) (i32.const 3))))
            (local.set $acc (i32.add (local.get $acc) (i32.const 1))))
          (local.set $acc (i32.add (local.get $acc) (i32.const 2))))
        (local.set $acc (i32.add (local.get $acc) (i32.const 4)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (local.get $acc)))
