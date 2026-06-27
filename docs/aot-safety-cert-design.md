<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — AOT Safety Certificate & Checker (M2 design)

> **Status:** Design proposal (Phase 2/3, the AOT long pole). Companion to
> [`aot-build-plan.md`](aot-build-plan.md) §4 and
> [`wasm-jit-design.md`](wasm-jit-design.md). This is the piece that lets
> Wari run AOT-compiled native code **without trusting the compiler** —
> the thing that keeps AOT compatible with the correctness/security
> ordering, the formal-verification path, and the MMU-free endgame.
> Per the Co-Architect Protocol the model choice is **Gustavo's call**;
> this lays out the options with trade-offs and a recommendation.

---

## 1 · The property to establish

For an AOT-compiled module (native RISC-V `.text` inside a WNM), prove —
on the device, before mapping it executable — that the code is
**software-fault-isolated (SFI)**:

1. **Memory isolation.** Every load/store the native code performs stays
   within the module's own linear memory (no escape to kernel/other-tenant
   memory). This is the load-bearing property; with it, compiled code is
   as confined as interpreted code.
2. **Control-flow safety.** Indirect branches/calls land only on a
   verified set of targets (a checked jump table + function entries); no
   jumps into the middle of instructions or outside `.text`.
3. **Bounded host transitions.** The only way out to the kernel is the
   sanctioned host-call trampoline (the cap-checked surface) — no raw
   `ecall`/syscall, no arbitrary kernel-address calls.
4. **Stack confinement.** Stack accesses stay within the instance's stack
   region.

If these hold, a compiler bug cannot produce a module that escapes its
sandbox, because the **device verifies the output**, not the compiler.

---

## 2 · Prior art that makes this tractable

**VeriWasm** (Johnson et al., NDSS 2021 — *Доверя́й, но проверя́й: SFI
safety for native-compiled Wasm*) is the key precedent and near-exactly
our situation:

- It is a **static, offline verifier of the native binary** produced by a
  Wasm→native compiler (**Lucet** — the same AOT model Wari adopts).
- It proves **SFI memory isolation post-compilation** by lifting machine
  code to a small IR and running **iterative dataflow / abstract
  interpretation** over an analysis lattice, per function.
- Crucially, it operates on the **compiler's output** and therefore does
  **not trust the compiler** — it independently re-establishes isolation.
  Soundness is proven; no false positives reported.

So the property is known-checkable on real Wasm-compiled binaries. The
open design question for Wari is **where the check runs and what artifact
it consumes** — which trades device-TCB size against compiler trust.

---

## 3 · Three models (the real fork)

| Model | Where the check runs | Device TCB cost | Trusts the compiler? | MMU-free-safe? |
|-------|----------------------|-----------------|----------------------|----------------|
| **A. Offline-verify + sign** | VeriWasm-style verifier runs **offline** in the signing pipeline; device trusts the **signature** asserting "passed verification" | tiny (just sig check) | no (trusts the *offline verifier* + signer, not the codegen) | **no** — device re-checks nothing |
| **B. On-device re-verify** | the full verifier runs **on the device** at load | **large** (lifter + dataflow + lattice in the kernel TCB; load-time cost) | no | yes |
| **C. Proof-carrying code (PCC)** | compiler emits **witnesses** (the abstract-interpretation facts); device runs a **small checker** that validates witnesses against `.text` | small (checking ≪ finding) | no | yes |

The classic PCC insight (model C): **checking a proof is far cheaper and
simpler than finding it.** The compiler does the hard analysis offline and
ships the result; the device only re-checks it — a small, verifiable
checker in the TCB, no compiler trust, and sound enough for the MMU-free
line where the verified output *is* the isolation.

---

## 4 · Recommendation: A now, C for the endgame

Phase the trust model to match the hardware line:

- **Phase 2/3 (MMU present): Model A — offline-verify + sign.** Run a
  VeriWasm-style SFI verifier in the offline pipeline; the WNM's
  `SafetyCert` section records a "verified-offline" attestation, signed
  with the existing envelope. The device checks the signature and maps
  RX-only. The **Sv39 MMU + PMP remain the hardware backstop**, so
  trusting the offline verifier + signer is acceptable, and the device
  side stays tiny. Fastest path to running AOT code safely.
- **Phase 4 (MMU-free endpoint): Model C — proof-carrying code.** When the
  MMU is removed, the verified output becomes the *primary* isolation, so
  the device must re-establish SFI itself — but cheaply. The compiler
  emits PCC witnesses into the `SafetyCert` section; a small on-device
  checker validates them. This checker joins the Phase-4 formal-
  verification scope alongside Tier-0 + `wasmi`.

The **WNM `SafetyCert` section is the carrier for both** — it already
exists in the format (`wari-wnm`); only its *contents* differ by model. So
choosing A first does not foreclose C: same format, richer payload later.

Model B (on-device full re-verify) is **rejected** for Wari — it puts a
large analyzer in the kernel TCB and pays it at every load, contradicting
the small-TCB thesis. C gets the same guarantee with a small checker.

---

## 5 · `SafetyCert` section contents (sketch)

The WNM `SafetyCert` section payload, versioned, by model:

- **Model A (attestation):** `{ verifier_id, verifier_version,
  wasm_hash, text_hash, verdict=PASS }`, covered by the envelope
  signature. The device checks: signature valid ∧ `text_hash` matches the
  mapped `.text` ∧ `wasm_hash` matches the embedded `.wasm`. No analysis
  on-device.
- **Model C (PCC witnesses):** per-function the facts the checker needs to
  re-validate §1 in one linear pass — e.g. the bounds-check/mask sites and
  their proven ranges, the verified indirect-branch target table, the
  set of call sites and that each targets the trampoline or a verified
  entry, and stack-extent facts. Format TBD with the verifier design; the
  goal is "checkable in one pass, no fixpoint iteration on-device."

Both forms are bounded-size and reproducible (R8): the same input must
yield the same cert so attestation is meaningful.

---

## 6 · What the on-device checker must establish (Model C)

A linear-pass checker over `.text` + witnesses confirms:

1. every memory access is preceded by a bounds check or uses a masked
   index provably within `[0, linmem_len)` (the §1.1 core);
2. every indirect branch is masked/checked into the verified target table
   (§1.2);
3. every call site targets the host-call trampoline entry or a verified
   function entry — never a raw kernel address or `ecall` (§1.3);
4. stack pointer adjustments keep accesses within the instance stack
   (§1.4);
5. the witnesses actually correspond to *this* `.text` (hash-bound), so a
   cert can't be transplanted onto different code.

If any check fails → reject the module (never map it). Fails closed.

---

## 7 · Integration points

- **WNM format** (`wari-wnm`): the `SafetyCert` section already exists;
  this design fills its payload. No format change for Model A; Model C
  adds a witness sub-format (a later `WNM_ABI_VERSION` bump if needed).
- **Signing envelope** (driver-iface pipeline): unchanged — the cert
  rides inside the signed WNM, one signing/attestation path.
- **Loader** (`load_plan` done): Model A → verify sig + hashes, map RX.
  Model C → additionally run the witness checker before mapping.
- **Offline pipeline** (`tools/wari-aot`, M1): hosts the VeriWasm-style
  verifier (Model A) and later the witness emitter (Model C).

---

## 8 · Decisions for the architect

1. **Confirm A-now / C-for-MMU-free** phasing (§4), or pick a single model.
2. **Build vs. adapt the verifier:** port/adapt VeriWasm's analysis (it
   targets x86-64 + Lucet; Wari is RV64 + Cranelift/our codegen — the
   lattice transfers, the lifter/backend specifics do not), or commission
   a fresh RV64 SFI verifier. Either is months — the AOT long pole.
3. **PCC witness format** (Model C, §5) — defer until A lands and the
   verifier exists, but it's the eventual cert-format decision the WNM
   loader is blocked on.

---

## 9 · Effort & first step

- Model A verifier (RV64 SFI, offline): the bulk — months; the natural
  home for the external/academic collaboration the build plan flags.
- First concrete step (after the M0 oracle + M1 Cranelift spike exist):
  run a **prototype SFI check offline** on one spike-compiled module —
  even a hand-checked property list — to validate that our codegen emits
  analyzable, isolatable code (bounds checks present, no wild indirect
  branches). That de-risks the whole verifier before investing in it.

This is a research-grade track; it should run **in parallel** with M0/M1
(per the build plan) precisely because it dominates the schedule.

---

## 10 · Prior art

| Pattern | Source | Role |
|---------|--------|------|
| Offline SFI verification of Wasm-compiled native code | **VeriWasm** (Johnson et al., NDSS 2021) | the model + proof the property is checkable |
| Near-zero-cost SFI transitions | **Isolation Without Taxation** (Kolosick et al., POPL 2022) | the host-call trampoline (§1.3) cost model |
| Proof-carrying code | Necula & Lee (1996–) | Model C: emit witnesses, check cheaply |
| AOT Wasm→native (the compiler this verifies) | **Fastly Lucet** | the codegen model Wari mirrors |
| Verified compilation / translation validation | **CompCert**, **Alive2** | the stronger optional layer above SFI |

Sources: [VeriWasm paper (UCSD)](https://cseweb.ucsd.edu/~dstefan/pubs/johnson:2021:veriwasm.pdf) ·
[VeriWasm repo](https://github.com/PLSysSec/veriwasm) ·
[Isolation Without Taxation](https://cseweb.ucsd.edu/~dstefan/pubs/kolosick:2022:isolation.pdf)

---

## 11 · Decision log

- **D1 — Verify the output, not the compiler.** The trust anchor is an
  SFI check over the native binary (VeriWasm-proven approach), not the
  codegen.
- **D2 — Phase the model to the hardware line:** A (offline-verify+sign)
  while the MMU is the backstop; C (proof-carrying code) for the MMU-free
  endpoint where the verified output is the primary isolation.
- **D3 — Reject on-device full re-verify (Model B):** too large for the
  kernel TCB; PCC gets the same guarantee with a small checker.
- **D4 — The WNM `SafetyCert` section carries the cert** in both models;
  choosing A first does not foreclose C (same format, richer payload).
- **D5 — Run this track in parallel** with M0/M1 — it is the AOT long
  pole and dominates the schedule.
