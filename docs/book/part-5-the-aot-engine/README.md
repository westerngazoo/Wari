# Part 5 — The AOT Engine

Two chapters on Wari's execution-strategy bet: compiling validated,
signed WASM to native RISC-V **ahead of time** (off-device, in the
signing pipeline) rather than interpreting it per-instruction, while
keeping the structural isolation the interpreter gave for free.

| Ch | Title | Grounds in |
|----|-------|-----------|
| 22 | Interpreter, JIT, or AOT | `docs/aot-build-plan.md`, `docs/aot-parallel-roadmap.md` |
| 23 | The Safety Certificate | `docs/aot-safety-cert-design.md` — running native code without trusting the compiler |

**In-progress snapshot.** The AOT track is under active construction;
these chapters describe the *design and the bet*, honestly flagging
what is decided vs. open (the M0 gate, the DG-1/2/3 decisions).
