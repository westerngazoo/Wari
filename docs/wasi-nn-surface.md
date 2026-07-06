<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — WASI-NN Surface (Lane B / B5)

> **Status:** Design proposal (Phase 2/3). The AI-compute path: how the
> AI-OS assistant's Planner (and any Tier-1 tenant) runs model inference.
> Companion to [`ai-os-assistant-design.md`](ai-os-assistant-design.md)
> and [`cap-registered-fastpath-design.md`](cap-registered-fastpath-design.md).
> Pure ABI enums/op-codes landed in `wari_abi::nn`; the runtime + driver
> path is Phase 2/3.
>
> **The point:** the WASM stays orchestration. The heavy math runs on the
> **GPU/GAPU accelerator** via a capability-gated host-fn surface. This is
> exactly why the WASM core doesn't need to be a supercomputer — and why
> "AI-first OS" doesn't mean "fast interpreter."

---

## 1 · Role in the system

The Planner is a control loop: decide → **infer** → act. `infer` is the
only compute-heavy step, and it leaves WASM entirely:

```
Planner (Tier-2 WASM)  ──wari_nn::compute──▶  accelerator driver (Tier-2)
   orchestration only                            GPU / GAPU — the math
        ▲                                              │
        └────────────── output tensor ◀────────────────┘
```

So the assistant's speed comes from AOT (orchestration) + accelerator
offload (inference), never from running a model in the interpreter.

---

## 2 · The surface (mirrors WASI-NN)

Five host fns under the `wari_nn` import module, shaped like the WASI-NN
proposal so standard toolchains emit compatible imports:

| Host fn | `wari_abi::nn` op | Meaning |
|---------|-------------------|---------|
| `nn_load(model_cap_slot, encoding, target) -> ctx_or_errno` | `NN_LOAD` | Load an **attested model capability** into an inference graph on `target`. |
| `nn_init_context(graph) -> ctx` | `NN_INIT_CONTEXT` | Create an execution context. |
| `nn_set_input(ctx, index, tensor_ptr, tensor_len, ty, dims_ptr) -> errno` | `NN_SET_INPUT` | Bind an input tensor from linear memory. |
| `nn_compute(ctx) -> errno` | `NN_COMPUTE` | Run inference — delegates to the accelerator driver. |
| `nn_get_output(ctx, index, out_ptr, out_len) -> written_or_errno` | `NN_GET_OUTPUT` | Read an output tensor back to linear memory. |

Enums (in `wari_abi::nn`, stable discriminants):
- **`GraphEncoding`** — Openvino/Onnx/Tensorflow/Pytorch/**Ggml**/Autodetect.
  `Ggml` is the likely on-board path (quantized llama.cpp-family LLMs).
- **`ExecutionTarget`** — Cpu/Gpu/**Gapu** (Wari adds the FPGA coprocessor).
- **`TensorType`** — F16/F32/I32/U8/I8.

---

## 3 · Capability model (the Wari adaptation)

WASI-NN's `load` takes raw model bytes; **Wari makes the model a
capability**:

- A **`Model` cap** references *attested* weights (signed/measured at
  provisioning, like a Tier-2 blob). `nn_load` takes a CSpace slot holding
  a `Model` cap — a tenant can only load models it was granted. Weights
  never transit untrusted WASM linear memory; the driver reads them from
  the attested store.
- An **inference context** is a derived capability (an `ExecCtx` object)
  minted into the caller's CSpace on `nn_load`/`nn_init_context`, revoked
  on drop. So a subverted Planner can only run models it holds and can't
  forge a context (INV-15/17 apply as for any cap).
- `nn_compute` on GPU/GAPU requires the driver hold the accelerator MMIO
  capability (Tier-2 grant) — the tenant never touches the device.

This keeps the Planner's WASI-NN access inside the same
least-privilege/attenuation story as everything else it does (see
`ai-os-assistant-design.md`): the model set is part of the per-task
attenuated caps the Supervisor mints.

---

## 4 · Execution path

1. `nn_load(slot, Ggml, Gapu)`: kernel `check_cap`s a `Model` cap at
   `slot`, hands the attested model id + target to the Tier-2 accelerator
   driver, which loads the graph and returns a graph/context handle.
2. `nn_set_input`: the input tensor lives in the caller's linear memory;
   the kernel bounds-checks `[tensor_ptr, tensor_ptr+tensor_len)` (wasmi
   enforced) and copies/maps it to the driver.
3. `nn_compute`: delegates to the driver, which runs the graph on the
   GPU/GAPU. The heavy math is entirely off-WASM, off-CPU.
4. `nn_get_output`: driver result copied back into the caller's linmem,
   bounds-checked.

Compute never runs in the interpreter or in Tier-0 — it's a
capability-mediated call to the accelerator driver.

---

## 5 · Batching via the cap fast path

An inference step is `set_input × N → compute → get_output × M` — a
natural fit for the **submission ring** (B1): register the `ExecCtx`
handle once, then batch the tensor ops through `ring_submit` so the
Planner's inner loop pays one kernel crossing per batch, not per tensor.
The ring's v1 op set extends with `NN_SET_INPUT`/`NN_COMPUTE`/
`NN_GET_OUTPUT` (each `op_permitted_for` an `ExecCtx` cap) — the same
validate-once-reference-many discipline, applied to inference.

---

## 6 · Decisions for the architect

1. **First encoding + target:** recommend `Ggml` on `Gpu` first (quantized
   LLMs, the sovereign-AI headline), `Gapu` in Phase 3. Gates the driver.
2. **Model provisioning:** how attested weights land on the board and get
   a `Model` cap (signing pipeline extension). Ties to the Tier-2
   attestation path.
3. **Streaming outputs:** LLM token streaming vs single `get_output` — do
   we need a `nn_get_output_stream` / a completion notification? (Lean:
   start single-shot; add streaming when the assistant loop needs it.)

---

## 7 · Prior art

| Pattern | Source | Role |
|---------|--------|------|
| `load` / `init_execution_context` / `set_input` / `compute` / `get_output` | **WASI-NN proposal** (W3C/BA) | the surface shape + encodings/targets/tensor types |
| Quantized LLM inference on-device | **llama.cpp / GGML** | the `Ggml` encoding, the likely on-board model format |
| Accelerator as a capability-gated driver | Wari Tier-2 model + **AWS Nitro** analog | GPU/GAPU behind a signed Tier-2 driver |
| Inference offload keeps the sandbox thin | Fastly/Cloudflare edge-AI | why the WASM core needn't be fast |

---

## 8 · Decision log

- **D1 — Inference offloads to the accelerator; WASM stays orchestration.**
  The AI-compute path is a host-fn surface, not a fast interpreter.
- **D2 — Models are attested capabilities**, not raw bytes in linmem — the
  Wari adaptation of WASI-NN `load`. Fits least-privilege/attenuation.
- **D3 — Mirror WASI-NN op/enum shapes** for toolchain compatibility; add
  `Gapu` execution target + treat `Ggml` as the on-board default.
- **D4 — Batch tensor ops through the cap fast-path ring** (B1) — one
  crossing per inference step, not per tensor.
