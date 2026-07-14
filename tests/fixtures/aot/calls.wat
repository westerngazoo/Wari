;; What it represents: Deep call graph / indirect calls
;; Provenance: Hand-written fixture for Wari AOT oracle
;; Expected observable behavior: Returns a calculated value from deep calls.
(module
  (type $sig (func (param i32) (result i32)))
  (table 2 funcref)
  (elem (i32.const 0) $add_one $sub_one)

  (func $add_one (param $n i32) (result i32)
    (i32.add (local.get $n) (i32.const 1))
  )

  (func $sub_one (param $n i32) (result i32)
    (i32.sub (local.get $n) (i32.const 1))
  )

  (func $deep (param $n i32) (param $depth i32) (result i32)
    (if (result i32) (i32.eq (local.get $depth) (i32.const 0))
      (then (local.get $n))
      (else
        (call_indirect (type $sig)
          (call $deep (local.get $n) (i32.sub (local.get $depth) (i32.const 1)))
          (i32.and (local.get $depth) (i32.const 1))
        )
      )
    )
  )

  (func (export "_start") (result i32)
    (call $deep (i32.const 100) (i32.const 10))
  )
)
