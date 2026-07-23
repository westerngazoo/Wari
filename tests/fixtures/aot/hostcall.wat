;; What it represents: Host-fn round-trip density (AI-assistant orchestration shape)
;; Provenance: Hand-written fixture for Wari AOT oracle
;; Expected observable behavior: Calls a host function multiple times and returns a value.
(module
  (import "wari" "yield" (func $yield (param i32) (result i32)))
  (func (export "_start") (result i32)
    (local $i i32)
    (local $sum i32)
    (local.set $i (i32.const 0))
    (local.set $sum (i32.const 0))
    (loop $loop
      (local.set $sum (i32.add (local.get $sum) (call $yield (local.get $i))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $loop (i32.lt_s (local.get $i) (i32.const 10)))
    )
    (local.get $sum)
  )
)
