;; What it represents: Integer hot loop (compute bound)
;; Provenance: Hand-written fixture for Wari AOT oracle
;; Expected observable behavior: Returns a fixed integer after some iterations.
(module
  (func (export "_start") (result i32)
    (local $i i32)
    (local $sum i32)
    (local.set $i (i32.const 0))
    (local.set $sum (i32.const 0))
    (loop $loop
      (local.set $sum (i32.add (local.get $sum) (local.get $i)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $loop (i32.lt_s (local.get $i) (i32.const 1000)))
    )
    (local.get $sum)
  )
)
