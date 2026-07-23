;; What it represents: Linear memory load/store churn
;; Provenance: Hand-written fixture for Wari AOT oracle
;; Expected observable behavior: Returns the final value written to memory.
(module
  (memory 1)
  (func (export "_start") (result i32)
    (local $i i32)
    (local.set $i (i32.const 0))
    (loop $loop
      (i32.store (local.get $i) (local.get $i))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br_if $loop (i32.lt_s (local.get $i) (i32.const 1024)))
    )
    (i32.load (i32.const 1020))
  )
)
