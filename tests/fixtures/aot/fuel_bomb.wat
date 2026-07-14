;; What it represents: Infinite loop (for fuel-path parity later)
;; Provenance: Hand-written fixture for Wari AOT oracle
;; Expected observable behavior: Exhausts fuel and traps.
(module
  (func (export "_start") (result i32)
    (loop $loop
      (br $loop)
    )
    (i32.const 0)
  )
)
