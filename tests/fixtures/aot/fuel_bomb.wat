;; fuel_bomb.wat
;; Provenance: Wari AOT Workload Corpus
;; Expected behavior: Infinite loop designed to consume all available execution fuel.
;; Tests the engine's fuel metering and trap handling capabilities.
(module
  (func $bomb (export "bomb")
    (loop $l
      (br $l)
    )
  )
)
