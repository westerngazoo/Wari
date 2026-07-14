;; calls.wat
;; Provenance: Wari AOT Workload Corpus
;; Expected behavior: Tests deep call graph and indirect calls.
;; Stresses function prologue/epilogue and call stack performance.
(module
  (type $sig (func (param i32) (result i32)))
  (table 2 funcref)
  (elem (i32.const 0) $f1 $f2)

  (func $f1 (param $x i32) (result i32)
    (i32.add (local.get $x) (i32.const 1))
  )

  (func $f2 (param $x i32) (result i32)
    (i32.mul (local.get $x) (i32.const 2))
  )

  (func $deep_call (export "deep_call") (param $depth i32) (result i32)
    (if (result i32) (i32.eqz (local.get $depth))
      (then (i32.const 0))
      (else
        (i32.add
          (i32.const 1)
          (call $deep_call (i32.sub (local.get $depth) (i32.const 1)))
        )
      )
    )
  )

  (func $indirect_call (export "indirect_call") (param $iters i32) (result i32)
    (local $i i32)
    (local $acc i32)
    (loop $l
      (local.set $acc 
        (call_indirect (type $sig) 
          (local.get $acc) 
          (i32.rem_u (local.get $i) (i32.const 2))
        )
      )
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (local.get $iters)))
    )
    (local.get $acc)
  )
)
