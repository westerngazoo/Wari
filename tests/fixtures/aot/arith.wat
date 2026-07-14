;; arith.wat
;; Provenance: Wari AOT Workload Corpus
;; Expected behavior: Performs a hot loop of integer arithmetic operations.
;; Should be efficiently compiled to machine code with tight loops.
(module
  (func $hot_loop (export "hot_loop") (param $iters i32) (result i32)
    (local $i i32)
    (local $acc i32)
    (loop $l
      (local.set $acc (i32.add (local.get $acc) (i32.const 1)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (local.get $iters)))
    )
    (local.get $acc)
  )
)
