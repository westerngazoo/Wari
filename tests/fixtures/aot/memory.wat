;; memory.wat
;; Provenance: Wari AOT Workload Corpus
;; Expected behavior: Performs heavy linear memory load and store operations.
;; Should test memory access performance and bounds checking elimination if applicable.
(module
  (memory (export "mem") 1)
  (func $mem_churn (export "mem_churn") (param $iters i32)
    (local $i i32)
    (loop $l
      ;; Store $i at address $i * 4
      (i32.store (i32.mul (local.get $i) (i32.const 4)) (local.get $i))
      ;; Load from address $i * 4
      (drop (i32.load (i32.mul (local.get $i) (i32.const 4))))
      
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      ;; loop until we reach iterations or max out available memory (1 page = 65536 bytes, so 16384 i32s)
      (br_if $l (i32.and (i32.lt_u (local.get $i) (local.get $iters)) (i32.lt_u (local.get $i) (i32.const 16384))))
    )
  )
)
