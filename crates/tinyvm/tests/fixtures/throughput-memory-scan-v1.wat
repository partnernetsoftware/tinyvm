;; Interpreter throughput workload: linear-memory traffic.
;; Every load and store re-checks bounds against the live memory slot, so this
;; is the row a bounds-check change has to prove itself on. The stored value is
;; derived from the trip index alone, so a persistent instance whose memory
;; survives between calls still returns the same answer every call.
(module
  (memory 1)
  (func (export "run") (result i32)
    (local $i i32) (local $addr i32) (local $acc i32)
    (block $done
      (loop $again
        (br_if $done (i32.ge_s (local.get $i) (i32.const 20000)))
        (local.set $addr (i32.and (i32.mul (local.get $i) (i32.const 4)) (i32.const 65532)))
        (i32.store (local.get $addr) (i32.add (local.get $i) (i32.const 7)))
        (local.set $acc (i32.add (local.get $acc) (i32.load (local.get $addr))))
        (local.set $acc (i32.xor (local.get $acc) (i32.load8_u (local.get $addr))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $again)))
    (local.get $acc)))
