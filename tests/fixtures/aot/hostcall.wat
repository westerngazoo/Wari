;; hostcall.wat
;; Provenance: Wari AOT Workload Corpus
;; Expected behavior: High density of host function calls to test context switching.
;; Tests overhead of entering and leaving WASM execution.
(module
  (import "env" "host_func" (func $host_func (param i32) (result i32)))
  (func $host_call_density (export "host_call_density") (param $iters i32) (result i32)
    (local $i i32)
    (local $acc i32)
    (loop $l
      (local.set $acc (call $host_func (local.get $acc)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (local.get $iters)))
    )
    (local.get $acc)
  )
)
