# Part 4 — Writing Drivers

The developer manual for authoring a Tier-2 (system) WASM driver:
the contract, the platform-selection system, signing, the build
pipeline, and a full bring-up war story. Grounded in the net and UART
drivers and in `docs/writing-a-tier2-driver.md`.

| Ch | Title | Covers |
|----|-------|--------|
| 18 | Anatomy of a Tier-2 Driver | trait + macro, `cfg`/features, MMIO/MDIO via host fns |
| 19 | The Manifest & Signing | the driver contract, the bidirectional sign check |
| 20 | Build, Embed, Flash | the pipeline, the stale-driver guard, the release flow |
| 21 | War Story: The Net Driver | GMAC bring-up, the RGMII delay hunt, reading the trace |
