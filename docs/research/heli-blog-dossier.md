Stubs — fine, the rest of the docs cover it. I have enough to write the dossier.

---

# Dossier: 和力 · Heli (Wari) — research base for the convergence-thesis blog post

Prepared from `/Users/goose/projects/wari` at commit `b6c0a43` (build 121). Numbers reflect the tree on disk; the rebrand from "Wari" to "和力 · Heli" is brand-layer only — every file, identifier, and on-wire string still says `wari`.

---

## 1. Executive summary

- Heli is a **WASM-native, capability-secured RISC-V operating system** under AGPL-3.0. There is no Linux inheritance, no ELF in the customer ABI, no third-party C in the TCB except the OpenSBI shim inherited from upstream firmware. Build 121, May 2026: boots on QEMU `virt` and on StarFive VisionFive 2 silicon; Phases 0/1a/1b shipped; Phase 1c (network) at silicon bring-up.
- **The thesis is structural, not aesthetic.** Three civilizational traditions — Andean *Ayni*, Mexican *Tequio*, Chinese *和 Hé* — independently encode the same principle: "every node has I/O obligations to the network; the network's health is cosmic law." Heli implements that principle as kernel mechanism: capabilities (every relationship is a token), two-tier WASM sandbox (membership is paid in audit), explicit IPC (no shared state).
- **Three sharpest defensible claims.**
  1. *Two independent isolation layers per tenant from day one* — the WASM validator + Sv39 MMU. An escape requires breaking both. Linux+Docker has one (cgroups+namespaces on a 30M-line kernel); Firecracker has one (KVM on a ~50KLOC VMM + a Linux guest). Heli stacks two structurally different mechanisms (`docs/security-model.md`).
  2. *Drivers are signed WASM*, not binary blobs. The NIC driver is a 2,949-line Rust crate compiled to `wasm32-unknown-unknown`, ed25519-signed, verified by the kernel before instantiation (INV-13), and run inside `wasmi` with its only hardware access mediated by capability-gated host fns (`drivers/net/src/lib.rs`, `kernel/src/runtime/sign.rs`). No comparable production OS treats drivers this way.
  3. *Capability system is seL4-shaped and Kani-proved on the critical mints*, not retrofitted. INV-10/15/16/17/18 are property-checked in `kernel/src/cap/proofs.rs` today, not on a wish-list (`docs/cap-system-design.md` §6, §8.3).
- **Sovereignty case.** Open ISA (RV64GC, no licensing), open silicon roadmap (JH7110 today → CoVE-enabled silicon Phase 3 → MMU-free custom SoC option Phase 4), AGPL-3.0, no telemetry, no undocumented interfaces, every driver auditable in source. The legal jurisdiction stays where the operator chooses to put the box; there is no out-of-band management plane.
- **What is honestly speculative.** Phase 3 GAPU FPGA, RISC-V CoVE integration, formal verification beyond the existing Kani harnesses, and the MMU-free SoC are designed and cited but not built. The Tier-2 net driver works at MMIO level on silicon but the YT8531C RGMII delay fix in build 121 has not been silicon-tested as of 2026-05-14 (`docs/STATE-OF-PLAY.md`).
- **The honest weaknesses.** Kernel/src is currently **8,784 lines of Rust + ~200 lines of asm**, not 5,000. wasmi is a pure interpreter — no JIT until Phase 2+; cold-start budget is **<10 ms target**, not microseconds. Smoltcp inside the net driver is ~30 KLOC of TCB outside the kernel that an auditor still has to read. No production tenants. No HTTP service yet. No external audit yet.
- **Why this is publishable now.** The cultural-technical convergence is the original frame. Every comparable project is either pure tech (RedLeaf, ATMO, seL4) or pure sovereignty rhetoric (most "sovereign cloud" pitches). Heli ties one to the other through an architecture whose primitives map line-for-line to the cultural principles.

---

## 2. Architecture outline

### 2.1 Narrative

Heli runs in three tiers on a single RV64GC hart (Phase 0–1; SMP deferred to Phase 2+):

- **Tier 0 — native Rust kernel, S-mode, no_std.** Boot, trap dispatch, Sv39 MMU, PLIC IRQ routing, scheduler, capability table, IPC, `wasmi` runtime, host-function dispatch. Only third-party dependency in the TCB is `wasmi 0.32.3` (pure interpreter, no_std). Compiled to RISC-V; `make verify` enforces single-build-tag coherence across all four artifacts (kernel + UART driver wasm + net driver wasm + hello wasm).
- **Tier 2 — signed WASM drivers, S-mode.** UART driver (`drivers/uart/`, 147 LOC) and net driver (`drivers/net/`, 2,949 LOC). Each is built as a separate cargo crate targeting `wasm32-unknown-unknown`, ed25519-signed against the kernel's compiled-in `ACCEPTED_PUBKEY` (INV-13), and `include_bytes!`-embedded into the kernel. Drivers reach hardware exclusively through capability-gated `wari::*_mmio_*` host functions. The validator (`kernel/src/validate.rs::is_*_mmio_addr`) narrows allowed addresses to the platform-specific NIC/UART window.
- **Tier 1 — customer WASM, U-mode.** Today: `apps/hello/`, plus integration test fixtures. Designed for 10,000–50,000 instances per board with <10 ms cold start (`docs/roadmap-and-architecture.md`). Tier 1 talks to drivers only via cap-mediated IPC mediated through `wari::*` host functions; never touches MMIO directly.

Boot chain: **OpenSBI (M-mode) → U-Boot → Wari kernel (S-mode) → wasmi → Tier-2 drivers → Tier-1 PID 1**.

Privilege is a per-module capability, not a language barrier. Every cross-tier call is gated by a capability the caller holds; every capability is unforgeable (Rust privacy on `Cap`, syscall trampoline checks INV-18 bounds, generation counter prevents ABA via INV-17). The cap system is seL4-shaped with Wari-specific simplifications documented in `docs/cap-system-design.md` §2.

### 2.2 ASCII diagram

> ```
>  +---------------------------------------------------------------+
>  |  Tier 1 — Customer WASM     (U-mode, MMU + WASM sandbox)      |
>  |    [hello.wasm]  [app.wasm]  ...  target 10k–50k/board        |
>  |    sees: WASI host fns only, no MMIO, no IPC except via caps  |
>  +-------------------|---------|---------------------------------+
>                      |  WASI host fn  (capability-gated)
>                      v
>  +---------------------------------------------------------------+
>  |  Tier 2 — System WASM       (S-mode, WASM-only sandbox)       |
>  |    [uart.wasm signed]  [net.wasm signed]                      |
>  |    drives HW via wari::*_mmio_* host fns; runs smoltcp        |
>  |    in-driver for net — TCP/IP never enters the kernel TCB     |
>  +-------------------|---------|---------------------------------+
>                      |  cap-gated IPC dispatch
>                      v
>  +---------------------------------------------------------------+
>  |  Tier 0 — Native Rust Kernel  (S-mode, no_std)                |
>  |   boot.S+boot.rs | trap.S+trap.rs | Sv39 page tables          |
>  |   PLIC dispatcher | scheduler | wasmi 0.32.3 interpreter      |
>  |   capability table (CSpace, 256 slots/proc, 16-B Cap)         |
>  |   IPC (Endpoint + Notification objects) | host-fn dispatch    |
>  |   ed25519 signature verify on every Tier-2 load               |
>  +---------------------------------------------------------------+
>                      |
>                      v
>  +---------------------------------------------------------------+
>  |  Hardware — JH7110 (Phase 0) / + GAPU FPGA + CoVE (Phase 3)   |
>  |   U74 cores | Sv39 MMU+PMP | Zkn/Zks crypto (P2)              |
>  |   CoVE confidential RAM (P3) | PCIe (P3 GAPU/GPU)             |
>  +---------------------------------------------------------------+
>
>  Three independent isolation layers per Tier-1 tenant:
>    1. STRUCTURAL  — wasmi validator + WASM type system
>    2. HARDWARE    — Sv39 page tables, then PMP (P1), then CoVE (P3)
>    3. CRYPTOGRAPHIC — Zkn/Zks at-rest + BLAKE3 in-flight (P2)
> ```

### 2.3 LOC table (counted from disk, build 121)

| Component | LOC | Path | Notes |
|---|---|---|---|
| Kernel Rust (`kernel/src/`) | 8,784 | `kernel/src/**/*.rs` | Tier 0 TCB |
| Kernel asm | 197 | `kernel/src/{boot,trap}.S` | Privileged entry |
| Memory subsystem crate | 1,301 | `wari-mem/src/` | Pure-logic Sv39 + page_alloc, host-testable |
| ABI source-of-truth | 327 | `abi-shared/src/lib.rs` | Syscall numbers + errors |
| WASI surface | 44 | `wasi/src/` | Preview1 subset + Wari ext |
| UART driver (Tier 2) | 147 | `drivers/uart/src/lib.rs` | wasm32 target |
| Net driver (Tier 2) | 2,949 | `drivers/net/src/lib.rs` | wasm32; embeds smoltcp |
| Tests (security + integration + fuzz) | 938 | `tests/` | 4-layer test discipline |
| Cap system within kernel | 4,128 | `kernel/src/cap/` | ~47% of kernel; load-bearing |
| Runtime within kernel | 2,358 | `kernel/src/runtime/` | wasmi embed + WASI + sign + Tier-2 dispatch |

**Honest TCB framing for the blog.** The "5–10 KLOC kernel" target is the *long-run* shape (`CLAUDE.md`). Today the kernel is **~9 KLOC of Rust**, of which **~4.1 KLOC is the capability subsystem** by design (seL4-grade caps are the load-bearing security mechanism; cf. cap-system-design.md §9 "the largest single subsystem in Wari to date"). Add `wari-mem` (1.3 KLOC pure-logic, host-testable, no unsafe outside two install paths) and `abi-shared` (0.3 KLOC pure data) and the audit surface is ~10.4 KLOC. By comparison: Linux 6.x is ~30M LOC; Firecracker VMM is ~50 KLOC *on top of* a Linux guest; seL4 is ~10 KLOC C + ~200 KLOC Isabelle proof.

### 2.4 Syscall surface

Exactly **20 syscall numbers**, source-of-truth in `abi-shared/src/lib.rs`:

| Group | Syscalls | Count |
|---|---|---|
| Core I/O | `PUTCHAR`, `EXIT`, `WAIT`, `GETPID`, `YIELD`, `REBOOT` | 6 |
| IPC | `SEND`, `RECEIVE`, `CALL`, `REPLY` | 4 |
| Memory | `MAP`, `UNMAP`, `ALLOC_PAGES`, `FREE_PAGES` | 4 |
| IRQ | `IRQ_REGISTER`, `IRQ_ACK` | 2 |
| Cap mgmt (Phase 1b) | `CAP_MINT`, `CAP_COPY`, `CAP_REVOKE`, `CAP_DELETE`, `CAP_LOOKUP` | 5 (slot 16 retired = `SYS_SPAWN_ELF`; never re-issued — R7) |

Compare: Linux ≥400 syscalls; gVisor implements ~250; the Firecracker guest still uses Linux's full surface; Cloudflare Workers exposes V8's full JS API. Heli's ABI is small enough that an auditor can fit the entire trap-dispatch table on one screen.

### 2.5 Comparison table — vs. alternatives

| Property | Linux + Docker | Firecracker microVM | MirageOS unikernel | Wasmtime + Linux | goose-os (predecessor) | **Heli (Wari)** |
|---|---|---|---|---|---|---|
| Tenant boundary | cgroup + namespace | KVM VCPU + Linux guest | none (1 tenant/img) | wasmtime instance | Sv39 + ELF process | **wasmi + Sv39** (double) |
| TCB lines (audit surface) | ~30M | ~50K + Linux | ~50K + app | wasmtime + ~30M | ~3 KLOC kernel (ELF) | **~9 KLOC kernel + 30 KLOC smoltcp in Tier-2** |
| Kernel language | C | Rust (VMM) + C (guest) | OCaml | C + Rust | Rust | **Rust no_std** |
| Driver model | kernel modules (C) | virtio passthrough | linked-in libraries | host OS drivers | userspace ELF servers | **signed WASM modules** |
| Cap system | none (DAC + LSM bolt-on) | none in VMM | n/a | none | none (Phase pre-cap) | **seL4-shaped, Kani-proved on mints** |
| Cold start (claim) | seconds (Spring Boot) | ~125 ms | ~20 ms | <10 ms (JIT) | ~ms (ELF exec) | **<10 ms target (interp.)** |
| ELF in customer ABI | yes | yes (Linux guest) | n/a | yes (host) | yes | **no, ever (R7)** |
| Open ISA path | x86/ARM (closed) | x86 (KVM) | any | host-dependent | RV64GC | **RV64GC, no exceptions** |
| Confidential compute | Intel TDX / AMD SEV | TDX / SEV | none | host-dependent | none | **CoVE Phase 3** |
| Formal verification | no | no | type-system inheritance | no | no | **Kani harnesses today; Verus path post-ATMO** |
| License | GPL+ misc | Apache-2.0 | ISC | Apache-2.0 | private | **AGPL-3.0-only** |

The defensible deltas: **double sandbox by construction** (only Heli stacks WASM validation on top of an MMU as the *default* per-tenant story); **drivers as signed WASM** (everyone else's drivers are either kernel C or unsignred binary blobs); **caps + ELF-banned + RV64-only** (nobody else combines all three).

### 2.6 Convergence mapping (Manifesto → primitive)

| Cultural principle | Architectural primitive | Where it lives |
|---|---|---|
| **Ayni** — every gift creates an obligation; the network is the standing balance of who owes what | **Capability tokens.** Every IPC, every MMIO read, every driver call is gated by an unforgeable `Cap` held in the caller's CSpace. The cap *is* the relationship. | `kernel/src/cap/types.rs` (Cap struct, 16 bytes, INV-15 forgery prevention); `kernel/src/cap/cspace.rs` (per-process table) |
| **Tequio** — membership in the network is paid in audit and contribution | **Two-tier sandbox.** Tier-2 drivers earn S-mode access by being signed (INV-13), manifested (INV-11), and verified on every load. No driver is ambiently trusted because it lives in the OS — every load is a fresh contribution-check. | `kernel/src/runtime/sign.rs`; `kernel/src/runtime/loader.rs` |
| **Hé** — structural harmony from properly aligned, explicit relationships | **Explicit IPC.** No shared memory between processes; no implicit broker. Every cross-process exchange is a typed, labeled, capability-gated send/receive on an Endpoint or signal on a Notification. Relationships are declared in manifests and enforced at the boundary. | `kernel/src/cap/objects.rs` (Endpoint, Notification); `docs/cap-system-design.md` §3.5 (derivation), §5 (IPC sysnums 21/22) |

This isn't a metaphor laid on top of an unrelated codebase. The cap-derivation tree literally encodes "who owes a debt downstream from whom"; the manifest signature literally encodes "this module earned its standing"; the IPC discipline literally refuses shared state because alignment cannot survive ambient sharing. The trilingual gloss is editorial; the underlying single principle is the architecture.

---

## 3. Security advantages — claims with citations

Each claim is tied to a repo path, an INV-N, or a named prior-art reference.

### 3.1 Layered defense (structural × hardware × cryptographic)

**Claim.** Heli stacks three independently-broken-only sandboxes per tenant. A successful Tier-1 escape requires breaking **all three** simultaneously: the wasmi validator (structural), the Sv39 MMU + PMP (hardware), and (Phase 2+) the at-rest encryption (cryptographic).

**Citation.** `docs/security-model.md` §"Three layers, three sandboxes" — explicit table mapping mechanism → primary guarantee → what breaks it → phase. Linux relies on cgroups+namespaces in one kernel; Firecracker relies on KVM; only Heli's per-tenant story is structurally redundant by design.

**Honest caveat.** Layer 3b (Zkn/Zks at-rest) is Phase 2; Layer 3c (CoVE) is Phase 3. Phase 1 ships layers 1 + 2 + ed25519 signature gate on Tier-2.

### 3.2 No ELF in the customer ABI — ever (R7)

**Claim.** `SYS_SPAWN_ELF` slot 10 in the ABI is *intentionally retired and never reissued*. The customer-facing path admits only signed WASM. The kernel literally cannot be tricked into loading a native executable by an inbound RPC because the code path does not exist (Phase 0 exit criterion #5).

**Citation.** `abi-shared/src/lib.rs:15-19` — *"slot 10 (formerly `SYS_SPAWN_ELF`) is retired — see CLAUDE R7"*; `CLAUDE.md` §Absolute Rules R7; `CLAUDE.md` §Phase 0 Exit Criteria #5.

**Why this matters.** Every microVM / sandbox-on-top-of-Linux design has a code-execution primitive at the bottom (exec, load_elf_binary). That primitive is the lifetime source of LPE CVEs and breakout chains. Heli has none — the bottom-most code-load primitive is `wasmi::Module::new`, fed bytes that have passed signature check and the WASM validator.

### 3.3 Drivers as signed WASM (INV-13)

**Claim.** The network driver — the largest piece of Heli outside the kernel — is a 2,949-line Rust crate compiled to `wasm32-unknown-unknown`. It is ed25519-signed against the kernel's compiled-in pubkey; verification happens *before* wasmi constructs the module; failure halts the kernel in Phase 0. The driver's only hardware reach is via capability-gated `wari_net_mmio_*` host functions. MMIO addresses are bounds-checked by `kernel/src/validate.rs::is_net_mmio_addr` against the platform NIC window.

**Citations.** `kernel/src/runtime/sign.rs` (INV-13); `drivers/net/src/lib.rs` (2,949 LOC, wasm32 target); `docs/net-driver-design.md` §5.1 (host-fn surface); `kernel/src/runtime/host_fns.rs` (cap gate); `docs/invariants.md` per-file site for `host_mmio_write8` (INV-3 narrowing).

**No comparable production OS does this.** Linux modules are C with no language sandbox. Firecracker drivers are inside the Linux guest. MirageOS drivers are linked into the unikernel. Wasmtime+Linux drivers are host C. Heli is the only design where a driver's runtime safety story is "WASM validator + signature + cap gate + bounded MMIO," each independently checkable.

### 3.4 TCP/IP outside the kernel TCB

**Claim.** Smoltcp (~30 KLOC) runs *inside the Tier-2 net driver's WASM linear memory*, never inside the kernel. A TCP CVE in smoltcp affects one tenant's traffic, contained by the driver's WASM sandbox + the cap layer. By contrast, Linux's TCP stack is in the kernel and a TCP CVE is a kernel CVE.

**Citation.** `docs/net-driver-design.md` §2 "Why TCP/IP in Tier-2"; choice explicitly rejected the kernel-resident option because "adding it to Tier 0 pushes the kernel from ~8 KLOC to ~40 KLOC overnight, destroying the audit-in-a-week thesis."

### 3.5 Capability system, seL4-shaped, property-proved on mint

**Claim.** Phase 1b ships a capability table where the kernel is the only producer of `Cap` values (INV-15 capability forgery prevention; enforced by Rust privacy), rights cannot be amplified through a mint chain (INV-10 monotonicity, Kani-checked in `cap::proofs::derive_preserves_rights_monotonicity`), derivation chains preserve kind/pool (INV-16), generation counters prevent ABA (INV-17), and every slot access is bounds-checked (INV-18).

**Citations.** `kernel/src/cap/types.rs` (Cap, derive); `kernel/src/cap/proofs.rs` (228 LOC of Kani harnesses); `docs/invariants.md` INV-10/15/16/17/18; `docs/cap-system-design.md` §6 + §8.3.

**Honest caveat.** Kani proves the *pure-logic mint primitive*. Full revoke-cascade and IPC-cap-transfer proofs are scoped to the Verus track in Phase 4 (`docs/cap-system-design.md` §11, referencing the Mars Research Group's ATMO/Verus work — `docs/research/atmo-sosp-2025-review.md`).

### 3.6 Single point-of-truth for the ABI (R8 + INV-1)

**Claim.** Syscall numbers and error codes live in exactly one place: `abi-shared/src/lib.rs`. No mirror files (a known goose-os bug source). The crate is pure data, host-testable, audit-exempt for unsafe (`docs/invariants.md` §"Non-contributing crates").

**Citation.** `abi-shared/src/lib.rs:5-13` ("single source of truth"); `docs/invariants.md` audit-exempt table.

### 3.7 Build-time stale-driver guard

**Claim.** A common failure mode in WASM-host architectures is the host embedding a stale signed driver blob. Heli's kernel `build.rs` greps the embedded signed wasm for a `WARI-DRV-BUILD-TAG-N` rodata string and refuses to compile if `N != WARI_BUILD`. The operator-facing safety net is `make verify`, which checks all four artifacts at the same tag.

**Citation.** `CLAUDE.md` §Build pipeline; `docs/STATE-OF-PLAY.md`§"Past lessons that matter" #1 (the build-107..114 bug that motivated the guard).

### 3.8 No telemetry, no undocumented interfaces

**Claim.** AGPL-3.0, public source, no out-of-band management plane. The boot chain (OpenSBI → U-Boot → Wari) is fully open and inspectable. There is no Intel ME, no AMD PSP, no proprietary firmware blob, no analytics ping.

**Citation.** `README.md` §"What Wari is"; `docs/prior-art.md` §"What we reject" (Intel SGX lineage rejected as cautionary tale about closed silicon).

### 3.9 Adversarial test discipline as a merge gate

**Claim.** Every new trust-boundary feature is blocked from merge until its `tests/security/` adversarial test exists. Current coverage includes malformed-WASM, OOM bomb, MMIO bypass, kernel-VA read.

**Citation.** `CLAUDE.md` §Security test suite; `tests/security/tests/` (oom_bomb.rs, page_fault_kill.rs); `docs/testing.md`.

**Honest caveat.** The cap-system adversarial tests enumerated in `docs/cap-system-design.md` §8.2 (`tier1_forge_cap.rs`, `tier1_amplify_rights.rs`, etc.) are *specified in the design* and partially implemented in `kernel/src/cap/proofs.rs`; the `tests/security/cap_*.rs` files are not all on disk yet.

---

## 4. Sovereignty / autonomy — the three-audience case

Each audience gets concrete, specific arguments — not "it's open source."

### 4.1 LATAM public-sector procurement officer

The buyer here is a CIO at a Mexican state health system, a Colombian central bank IT director, an Andean tax authority CTO. They have signed contracts that put citizen data under a Virginia or Dublin governance regime; they have read enough subpoena cases to know what that means; they need a stack their internal auditors can sign off on.

What Heli concretely provides that AWS/Azure on x86+Nvidia does not:

- **Jurisdictional clarity by construction.** The hardware is a StarFive VisionFive 2 (JH7110, RV64GC) on premises. There is no cloud-control-plane that observes the workload. No KMS key custody by a foreign entity. Compare AWS Nitro: even with VPC + customer-managed KMS, the hypervisor is closed-source firmware on closed silicon.
- **Auditable down to the driver.** Linux drivers are kernel C modules, often unsigned, often vendor blobs. Heli drivers are signed Rust compiled to WASM (`drivers/net/src/lib.rs`, `drivers/uart/src/lib.rs`); a procurement-side reviewer can run `cargo audit` + read the source + verify the signature chain. INV-13 says the kernel literally refuses to load an unsigned blob.
- **Continuity-of-service story.** The build is reproducible (R8): `rust-toolchain.toml` pins 1.95.0, `Cargo.lock` is committed, builds are bitwise-identical. A government can mirror the source, the toolchain, and the artifacts; if upstream development ever stops, the buyer holds a complete, buildable, signable snapshot.
- **No supply-chain hostage.** RISC-V is an open ISA — no per-core license, no export-control kill-switch, no proprietary microcode. SiFive, T-Head, StarFive, Nuclei, Rivos all build RV64 silicon; cross-vendor portability is the spec, not a vendor concession.
- **Formal-verification trajectory.** seL4 is the existence proof that a microkernel can be verified for the world's most security-sensitive deployments (military, aviation). Heli is built shape-compatible with that approach from day one (INV-N discipline, Kani harnesses, Verus-ready design per the ATMO review). An auditor in 2028 looking for "where is the derivation tree" finds it (`docs/cap-system-design.md` §3.5).

The pitch line: *"You buy the box, you own the box, you can read every line of code that runs on the box, and the math is on a path to be checkable by your auditors — not ours."*

### 4.2 The "I don't want my data on AWS" individual

The audience here is the developer who runs Mastodon at home, the journalist whose source lists live on a colocated box, the privacy-minded small-business owner. Their mental model is FreeBSD-on-a-NUC; Heli's offer is "the same posture but with stronger structural guarantees and an open ISA."

What Heli concretely provides that Debian/FreeBSD on x86 does not:

- **No Intel ME / AMD PSP equivalent.** The JH7110 boots OpenSBI from open ROM; there is no out-of-band management engine running on a hidden core. (The VF2 board itself is auditable; CPU dies are not yet open silicon, but they are not running a foreign-controlled OS.)
- **One config surface, not five.** Docker's security story requires Dockerfile + seccomp + AppArmor + cgroups + network policy to all agree (this was the explicit argument in the 2026-04-11 blog post). Heli's per-workload story is one signed manifest declaring the caps the workload is granted; the kernel enforces it. Audit reduces from "find the missing seccomp rule" to "read this 20-line manifest."
- **The driver pain goes away.** On Linux, every kernel upgrade risks a driver regression — proprietary GPU drivers, weird NIC firmware blobs, vendor-shipped binary modules. Heli drivers are signed WASM crates that the individual operator can rebuild from source against any kernel version that ships the same ABI.
- **Honest hardware footprint.** A VF2 is ~$80, 4 cores at 1.5 GHz, 8 GB RAM, sips power, sits silently in a closet. The target density (10k–50k Tier-1 instances per board) means a home-scale Mastodon plus a dozen side projects fit comfortably.

Caveat to surface honestly: today this user gets a kernel that boots, prints "Hello from Wari," runs a hello.wasm, and has a network driver under final bring-up. The HTTP service comes in Phase 2 once the socket host fns and oci2wasm land. This audience is for the *story*, not the *2026 deployment*.

### 4.3 Chinese long-cycle / sovereign-fund capital

The audience here is a family-office partner in Singapore, a state-aligned VC in Shenzhen, a Hong Kong PE fund with a 10-year IRR horizon. They have read the AWS Nitro patents; they understand that infrastructure of this kind compounds slowly and dominates terminally. They are also explicit about wanting alignment with the 一带一路 / sovereign-tech narrative without an explicit nation-state pitch.

What Heli concretely provides that aligns:

- **RV64 ISA is the open lane that all serious Chinese silicon investment is converging on.** T-Head (Alibaba), XuanTie, Nuclei, Rivos. A Rust kernel built shape-compatible with the entire RISC-V ecosystem is a pre-positioned asset on the side that is winning the multi-decade ISA fight outside Apple/x86.
- **AGPL-3.0 is the LICENSE that defeats hyperscaler enclosure.** Cloud providers cannot take Heli, run it as a service, and refuse to publish their modifications. The "built to be shared, not rented" tagline is operational — AGPL §13 forces source publication on network use.
- **Cultural-narrative fluency.** The 和 frame is not a translation layer over a Western project. The manifesto cites *Hé*, *Tequio*, and *Ayni* on equal footing; the brand is structurally Pacific (Andean + Mesoamerican + East Asian), not a renamed European import. This matters for procurement in Beijing, Mexico City, and La Paz in ways US-origin "sovereign cloud" pitches cannot match.
- **Dynastic time-horizon framing in the README.** "1,400-year-old validated social technology." "Chinese capital thinks dynastically." This isn't marketing — it is the literal time-horizon under which the formal-verification roadmap (Phases 3–4) makes sense. A 3–5 year IRR fund will not finance Verus proofs of wasmi; a 15-year fund will.
- **Hardware-software co-design as moat.** Phase 3 GAPU FPGA + CoVE + multi-board clustering. This is the AWS Nitro thesis (offload network/storage/security to dedicated silicon) realized on open ISA — and a place where Chinese FPGA / RISC-V silicon investment converges naturally.

The pitch line: *"This is the operating system the Pacific century needs — open ISA, AGPL kernel, signed-WASM drivers, formal-verification trajectory, with cultural fluency that lets the same artifact ship into Oaxaca, Lima, and Shenzhen without rebranding."*

### 4.4 What sovereignty does NOT mean here

To pre-empt the cheap-shot review:

- It does not mean "no foreign code." Wasmi is from the Bytecode Alliance (US); the Rust compiler is Mozilla-origin; OpenSBI is multi-vendor. The bet is on *auditable* foreign code (cited in `docs/prior-art.md`, version-pinned in `Cargo.lock`), not on autarky.
- It does not mean "Chinese-only" or "anti-American." The manifesto explicitly: *"Not anti-anything. 和力 does not require an enemy to define itself against."*
- It does not mean "we re-invented everything." Every architectural pattern is cited (`docs/prior-art.md`) — seL4 caps, Fastly WASM-as-boundary, Cloudflare shared-runtime density, Firecracker narrow-Rust discipline, Singularity managed-code OS. The bet is on *recombination*, not invention.

---

## 5. AI coprocessor architecture (Phase 3 GAPU)

This is the section most heavily extrapolated. The repo has the framing (`docs/architecture.md`, `docs/prior-art.md` "GAPU FPGA as architectural peer to GPU", `CLAUDE.md` Phase 3 roadmap line `Tier-2 GAPU FPGA driver over PCIe`) but no design doc on disk equivalent to `docs/net-driver-design.md`. I label extrapolations explicitly.

### 5.1 What the repo actually commits to (not speculative)

- **GAPU = "GPU-Analog Processing Unit"-style architectural peer.** Treated in Phase 3 as a *canonical AI-inference target*, not a "we also happen to support FPGA" afterthought (`docs/prior-art.md` §"What's genuinely our bet" #2).
- **Inference host fns follow the WASI-NN shape.** `wari_ai_infer` is to be shaped so Wari-built inference modules port to other WASI-NN hosts (`docs/prior-art.md` §WASI-NN).
- **Hardware lives behind PCIe.** Both GPU (Phase 2) and GAPU (Phase 3) drivers are Tier-2 WASM driving the device through PCIe MMIO (`docs/architecture.md` mermaid diagram, edge `D3/D4 -- PCIe/MMIO --> H5`).
- **Phase 3 also brings CoVE.** Confidential VM extension — ciphertext RAM per tenant. AI workloads are the natural beneficiary (model weights + inputs both stay encrypted in DRAM).

### 5.2 Extrapolated design — credible Phase-3 GAPU architecture

> *Everything below is extrapolation from the repo's framing, not committed code or design. Treat as a thesis-shaped strawman Gustavo can refine before publishing.*

**Hardware shape.** A multi-board RISC-V cluster (e.g. 4× VF2-class boards) plus one or more **GAPU FPGAs** attached over PCIe Gen3/4 to a designated "AI-host" board. The GAPU is a programmable accelerator (Xilinx/Lattice-class FPGA today; ASIC-class silicon Phase 4) running matrix-multiply / attention / convolution kernels appropriate to LLM and CNN inference. Bitstream is signed and loaded by the Tier-2 driver at boot.

**Software shape.**

> ```
>  Tier-1 customer WASM:    inference.wasm
>    |   wari::ai_infer(model_handle, input_buf, input_len,
>    |                  output_buf, output_max, cap_slot_for_model)
>    v
>  Tier-0 dispatch:  capability check on Socket-like Model cap
>    |   cap.kind == Model; cap.rights & EXECUTE; cap.gapu_idx valid
>    v
>  IPC into Tier-2 GAPU driver  (cap-gated, badged by tenant id)
>    |
>  Tier-2 GAPU driver (drivers/gapu/, signed wasm):
>    |   - validates input shape against model manifest
>    |   - DMA-marshals input from driver lin-mem to FPGA PCIe BAR
>    |   - rings doorbell MMIO; waits on Notification bound to GAPU IRQ
>    |   - reads output from PCIe BAR back into driver lin-mem
>    |   - returns to Tier-0 dispatch; Tier-0 marshals into Tier-1 lin-mem
>    v
>  Hardware: GAPU FPGA computes, raises completion IRQ
>             via PLIC -> Notification path (INV-23 lineage)
> ```

**Capability gating, four new `ObjectKind`s (extrapolation by analogy to net-driver-design.md §6):**

| Kind | Held by | What it authorizes | Mint path |
|---|---|---|---|
| `Gapu` | Tier-2 GAPU driver only | Use of the GAPU device itself, including bitstream load | Root cap minted by `cap::boot::init_root_caps` from the driver's signed manifest; Tier-1 mint returns `E_PERM` (INV-19) |
| `Model` | Tier-1 tenant | Inference against one specific loaded model | Minted by the GAPU driver in response to a tenant request, with rights bits `EXECUTE` and (optionally) `WEIGHTS_READ` for model-introspection workloads |
| `Bitstream` | Tier-2 GAPU driver only | Reprogram the FPGA fabric | Root cap; rotation requires re-signature of the bitstream blob |
| `ConfidentialModel` (Phase 3+, CoVE-gated) | Tier-1 tenant | Inference where model weights live in CoVE ciphertext RAM, invisible to kernel core-dump | Minted only when CoVE is initialized and a per-tenant confidential context exists |

**Tenancy story.** Model weights are loaded into the GAPU's HBM (or board RAM behind the FPGA) under a `Model` cap. Multiple tenants holding `Model` caps for the *same* underlying weights share the loaded copy; tenants with distinct models pay separate weight-load latency. Inference queue ordering is fair-scheduled by the GAPU driver; rights are checked per `ai_infer` call. Revoking a tenant's `Model` cap immediately stops dispatching their pending inferences.

**What this architecturally buys over "stick an Nvidia GPU on a Linux box":**

1. **Capability gating on inference itself.** A Tier-1 tenant cannot invoke arbitrary CUDA kernels; it can only invoke against models for which it holds a `Model` cap with `EXECUTE` rights. On Linux, any process with `/dev/nvidia*` access has full GPU command-buffer authority.
2. **Driver is signed WASM.** The GAPU driver is `wasm32` source under the same `INV-13` signature gate as the UART and net drivers. By contrast, the Nvidia driver is ~30M lines of closed kernel C+ASM living in the Linux TCB.
3. **Bitstream attestation by construction.** The bitstream is a signed blob; the kernel verifies the signature before the driver flashes the FPGA. There is no Linux equivalent — GPU firmware loads are vendor-signed but the kernel does not know what the firmware does.
4. **CoVE-encrypted weight residency (Phase 3+).** Model weights live in ciphertext RAM. A core-dump of the kernel leaks nothing about the model. This matters specifically for sovereign LLM deployments — a hosted model fine-tuned on a hospital's patient corpus cannot be exfiltrated by a memory-bus attacker.
5. **Multi-tenant inference quota gated by caps.** Per-tenant rate-limiting and fairness becomes a capability badge attribute, not a Linux cgroup hack.

### 5.3 Prior art consulted (named, deliberately adopted or rejected)

| Source | Adopt | Reject |
|---|---|---|
| **Apple Neural Engine** | Coprocessor-attached-to-SoC topology; per-process inference handles | Closed silicon, closed driver |
| **AWS Inferentia / Trainium** (Nitro lineage) | HW/SW co-design as strategy (this is `docs/prior-art.md` AWS Nitro inheritance, restated for AI) | Proprietary silicon, AWS-only |
| **Google TPU** | Systolic-array compute pattern as a Phase-4 GAPU bitstream target | Proprietary; XLA compiler dependency |
| **RISC-V T-Head / XuanTie AI extensions** (RVV + AI-specific instructions) | Worth adopting on Phase-3 host cores; aligns with the "Pacific-RISCV-stack" sovereignty pitch | None — this is upstream-aligned by default |
| **WASI-NN draft spec** | Host-fn shape (`wari_ai_infer`) follows so workloads port back to Wasmtime hosts | None — draft is still moving |
| **CHERI capability-on-pointer** | Phase 4 candidate for hardware-enforced cap bounds on FPGA-side memory | Phase 1b is software-enforced caps; CHERI is complementary, not substitute |

### 5.4 Open architectural questions Gustavo should resolve before the post commits

- **Single GAPU per cluster vs. one per board.** Centralized-attached is simpler; distributed is more fault-tolerant. The repo does not commit.
- **Model storage**: where do weights physically live? In the FPGA's HBM (fast, limited), in the host board's RAM (slow, large), in CoVE-protected RAM (slowest, encrypted, Phase 3)? Likely tiered.
- **Bitstream rotation cadence.** Is a tenant allowed to bring its own bitstream (e.g. a customer-specific inference kernel), or only run against driver-supplied bitstreams? Capability story changes meaningfully between the two.
- **Per-board AI vs. shared cluster AI.** Phase 3 also brings multi-board clustering. The GAPU could be a cluster-wide resource (cap delegated over distributed-cap IPC) or board-local.

---

## 6. Comparison vs. alternatives — extended

Beyond the headline table in §2.5, here are the longer-form comparisons most likely to come up in technical review or hacker-news comments.

### 6.1 vs. Linux + Docker

The reference case. Linux has 30M LOC and 400+ syscalls; Docker layers cgroups + namespaces + a runtime on top. The container-escape CVE history (CVE-2019-5736 runc, CVE-2022-0185 fs context, etc.) is the canonical argument for *not* using a 30M-line kernel as the tenant boundary. Heli's bet: collapse to ~10 KLOC of TCB with structurally redundant isolation.

What Heli gives up: any binary compatibility, ecosystem maturity, hardware support breadth, the entire `/proc` userland model, fork()+exec(), POSIX threads, NUMA-aware kernel features.

### 6.2 vs. Firecracker microVMs

Firecracker is the *closest aligned* commercial design — narrow-purpose Rust VMM, serverless target, AWS production. Heli inherits the discipline (`docs/prior-art.md` §Firecracker) and rejects the model (microVM-per-invocation is too heavy for 10k–50k tenants/board).

The hard comparison: Firecracker still runs a Linux guest. Heli has no guest OS — Tier-1 *is* the customer code, no kernel between it and wasmi. TCB-wise: Firecracker has VMM (~50 KLOC) + Linux guest (~30M); Heli has Tier-0 (~9 KLOC) + smoltcp-in-Tier-2 (~30 KLOC, sandboxed).

### 6.3 vs. MirageOS (unikernels)

MirageOS gives up process isolation entirely — every component is in one address space. The 2026-04-11 blog post already argued this; Heli keeps isolation because the cap system + MMU are the value proposition. Where MirageOS wins: tiny image size, instant boot, OCaml's type system as the safety net. Where Heli wins: multi-tenancy, signed driver discipline, formal-verification target on the kernel rather than the application.

### 6.4 vs. Wasmtime + Linux (the "WASM-on-a-real-OS" path)

This is the default everyone reaches for: run WASM as a userspace process on Linux. It works. It is fast. It is the Fastly architecture. What it lacks: hardware sovereignty (still x86 + Linux), structural isolation redundancy (only the WASM sandbox; the underlying kernel is the trust base), AGPL-clean licensing posture, and any path toward a verified kernel underneath. Heli is the bet that those four things matter enough to justify writing a kernel.

### 6.5 vs. goose-os (the predecessor)

goose-os is referenced throughout (`docs/prior-art.md`, `docs/invariants.md` cherry-pick lineage, `CLAUDE.md` §Current Status). It shipped 13 phases of an ELF-process microkernel on RV64 with UART servers, IPC, preemption, and a userspace driver story. Heli's relationship: cherry-pick pure-logic modules (`page_alloc.rs`, `page_table.rs`, the validator pattern, the INV-N framework from `docs/unsafe-audit.md`), rewrite everything shaped by ELF/native-process assumptions. The biggest delta: goose-os had `SYS_SPAWN_ELF`; Heli has retired that slot forever (R7).

The narrative continuity matters: the 2026-04-11 post introduced the WASM-native OS thesis under the goose-os name. Heli is the *evolution* of that thesis with the brand and architectural commitments hardened.

### 6.6 vs. Singularity revival (the long-shot endpoint)

Singularity (MSR, 2003–08) proved that language-enforced isolation without page tables is sound, and died for non-technical reasons. Heli's Phase 4 explicitly takes Singularity's architectural endpoint — *MMU-free custom silicon* with WASM as the sandbox — and updates the runtime choice from CLR to wasmi (smaller, cross-language, machine-verifiable). The bet: 2026 has the enablers (Verus, ATMO's proof economics, RISC-V CoVE, mature wasmi) that 2008 lacked.

This is the deepest moat if it works. It is also the most speculative claim and Gustavo should label it as such whenever it appears in print.

### 6.7 vs. ATMO (the academic sibling Heli will be compared to)

`docs/research/atmo-sosp-2025-review.md` is essential reading. ATMO (SOSP 2025 Best Paper, Mars Research Group) is a Verus-verified Rust microkernel with L4-class surface, ~7.5:1 proof-to-code ratio, ~2 person-years effort. Direct relevance: ATMO is the existence proof that the Verus-track Phase 4 formal-verification ambition is *practical*, not aspirational. Heli's cap-system design is shape-compatible with ATMO's endpoint model. Gustavo should expect every "verified kernel" comparison to surface ATMO; the right framing is "Heli is shaped to inherit ATMO's verification methodology when we get there."

---

## 7. What's shipped / designed / speculative — the honesty matrix

| Component | Status | Evidence |
|---|---|---|
| Kernel boots on QEMU `virt` RV64 | **Shipped** | `make run`, build 121 |
| Kernel boots on StarFive VF2 silicon (JH7110) | **Shipped Phase 1a** | `README.md` first-boot photo April 2026; `docs/book/part-3-phase-1a-silicon/` |
| Tier-2 UART driver (signed WASM) | **Shipped Phase 1b** | `drivers/uart/`, `kernel/src/runtime/tier2_uart.rs`, INV-13/14 |
| Tier-1 `hello.wasm` runs to `proc_exit(0)` | **Shipped Phase 1a** | `apps/hello/`, integration test `tests/integration/tests/runtime_noop.rs` |
| Capability system (mint/copy/revoke/delete/lookup) | **Shipped Phase 1b** | `kernel/src/cap/{types,cspace,objects,syscall,revoke,boot,proofs}.rs` totaling 4,128 LOC |
| Kani proofs on mint primitives | **Shipped Phase 1b** | `kernel/src/cap/proofs.rs` 228 LOC; INV-10/15/16/17/18 |
| Synchronous IPC (Endpoint + Notification) | **Shipped Phase 1b** | `kernel/src/cap/objects.rs`; sysnums 21/22 reserved |
| Sv39 MMU + identity-mapped kernel | **Shipped Phase 0** | `kernel/src/mem/kvm.rs`; INV-5, INV-6 |
| PLIC IRQ → Notification routing | **Shipped Phase 1b** | `kernel/src/mmio/plic.rs`; INV-23 |
| GMAC0 net driver: hardware path init | **Shipped (silicon) build 120** | `drivers/net/src/lib.rs` |
| ARP/ICMP reply path on silicon | **In progress, build 121 not silicon-tested** | `docs/STATE-OF-PLAY.md`; YT8531C RGMII delay calibration |
| TCP socket layer (smoltcp in Tier-2) | **Designed, Net-5/Net-6 PRs queued** | `docs/net-driver-design.md` §9.1 |
| Tier-1 echo demo app | **Designed Phase 1b PR Net-7** | `docs/net-driver-design.md` §8.4 |
| oci2wasm Docker→WASM compiler | **Designed Phase 2** | `CLAUDE.md` Phase 2 roadmap; `docs/prior-art.md` §Kata |
| Hardware crypto Zkn/Zks integration | **Designed Phase 2** | `docs/security-model.md` Layer 3b |
| WASI-NN host fn surface | **Designed Phase 2** | `docs/prior-art.md` §WASI-NN |
| GPU driver over PCIe | **Designed Phase 2** | `CLAUDE.md` Phase 2 roadmap |
| RISC-V CoVE confidential RAM | **Designed Phase 3** | `docs/security-model.md` Layer 3c; ratified spec, silicon 2026–27 |
| GAPU FPGA Tier-2 driver | **Designed Phase 3, no design doc on disk yet** | `CLAUDE.md` Phase 3; `docs/prior-art.md` §"What's genuinely our bet" #2 |
| Multi-board clustering / distributed caps | **Designed Phase 3, deferred from Phase 1b** | `docs/cap-system-design.md` §1 non-goals |
| Per-module WASM formal verification harness | **Designed Phase 3** | `CLAUDE.md` Phase 3 roadmap |
| External security firm audit | **Designed Phase 3 gate** | `docs/security-model.md` §Audit cadence |
| Kani proofs of capability monotonicity + scheduler invariants | **Designed Phase 4** | `CLAUDE.md` Phase 4 roadmap |
| wasmi correctness proof (academic collab) | **Speculative Phase 4** | `CLAUDE.md` Phase 4; depends on external partner |
| Hash-attested ROM kernel | **Speculative Phase 4** | `CLAUDE.md` Phase 4 |
| MMU-free custom silicon (Singularity endpoint) | **Speculative Phase 4+ option** | `docs/book/part-1-architecture/ch07-immutable-endpoint.md`; `CLAUDE.md` §Long-term endpoint |

The matrix above is the single most important table for the blog post. Gustavo can credibly claim everything in the "Shipped" rows; should label every "Designed" row as such; and should explicitly label "Speculative" rows as bets, not commitments.

---

## 8. Suggested narrative hooks for the blog post

Five distinct openings Gustavo can choose from. Each leads with a different rhetorical move; the architectural payload is the same.

1. **"Three civilizations, one principle, one kernel."** Open with the convergence thesis from `docs/manifesto.md`. Ayni, Tequio, Hé all encode "every node has I/O obligations to the network." Heli is the kernel where that principle is the mechanism, not the slogan. Then walk down: capabilities = ayni, two-tier sandbox = tequio, explicit IPC = hé. Strongest cultural-narrative hook; risks being dismissed as marketing without the technical follow-through. Pair with the LOC table early to anchor.

2. **"Why I retired syscall slot 10 and will never reissue it."** Open with the single line in `abi-shared/src/lib.rs` retiring `SYS_SPAWN_ELF`. Use that to explain R7 ("no ELF in the customer ABI, ever"), then build out: if the kernel can't load native binaries, what runs? WASM. How does it isolate? Double sandbox. How does it talk to drivers? Signed WASM drivers. Walk up to the convergence thesis at the end. Strongest engineer-credibility opening; the cultural framing comes as the "and why this is more than a tech project" reveal.

3. **"From goose-os to 和力 — what changes when you take WASM seriously from boot zero."** Continuity from the 2026-04-11 post. The earlier post said: imagine a kernel where every process is a WASM module. Heli is the rewrite that actually does it — no ELF anywhere, signed-WASM drivers, capabilities from Phase 1b. This is the natural blog-series-arc opening. Risk: requires readers to remember the prior post.

4. **"What sovereignty looks like at 8,784 lines."** Open with the kernel LOC count and the absurdity of the alternative (30M-line Linux as the trust base for a hospital's patient records in Oaxaca). Walk through what fits in 9 KLOC: boot, MMU, scheduler, caps, IPC, wasmi embed, host fns. Then explain why that smallness is the sovereignty story — auditable in a week by a team of three. The convergence frame comes at the end as "why a kernel this small is actually a civilizational bet, not a technical exercise." Strongest investor-pitch opening.

5. **"Drivers as signed WASM: the architecture nobody else ships."** Open with the net driver — 2,949 lines of Rust, compiled to wasm32, ed25519-signed, MMIO-bounded. Compare to the Linux driver model (kernel C modules), the microVM model (drivers in the guest), the unikernel model (drivers linked in). Then zoom out: this is what auditability actually means; this is what makes the sovereignty pitch concrete. Strongest "show, don't tell" technical opening.

Personal recommendation if Gustavo wants one: **hook 2** (the retired syscall slot) for the post itself, with **hook 1** (the convergence thesis) as the closing crescendo. That sequence earns the cultural frame on the back of demonstrated technical seriousness, which is the right order for the audiences in §4.

---

## 9. Open questions / things to verify before publishing

Checklist for Gustavo to walk through before the post goes live.

- [ ] **Net driver build-121 silicon test status.** `docs/STATE-OF-PLAY.md` says build 121 (YT8531C RGMII delay fix) was not yet flashed on VF2 as of 2026-05-14. If the post claims "Phase 1c silicon-bring-up working," confirm it actually pings. If not, soften to "ARP/ICMP path under final calibration."
- [ ] **Kernel LOC truth-in-advertising.** The "5–10 KLOC" target in `CLAUDE.md` and the marketing materials is the *long-run* shape. Today the kernel is ~9 KLOC, of which ~4.1 KLOC is the cap system. The post should not claim "5 KLOC kernel" without footnote. Suggested phrasing: *"~9 KLOC Rust kernel today; ~4 KLOC of which is the seL4-style capability subsystem."*
- [ ] **"Boots on VisionFive 2" — confirm last successful boot.** The April 2026 photo in `README.md` is the canonical evidence. If anything has regressed since, the post should be honest.
- [ ] **Phase 3 GAPU framing.** §5 of this dossier is the most speculative. Decide before publishing whether GAPU appears in the blog post at all. Two safe options: (a) defer GAPU to a follow-up post and lead Phase 3 with CoVE only, (b) include GAPU with explicit "designed, not built" labeling. Do not let GAPU read as shipped.
- [ ] **Verus / ATMO citation.** If the post mentions formal verification, cite ATMO (Mars Research Group, SOSP 2025 Best Paper) and reference `docs/research/atmo-sosp-2025-review.md`. Don't claim Heli is verified — claim it is *shaped to be verifiable on the ATMO methodology*.
- [ ] **License posture.** Confirm AGPL-3.0-only is still the chosen license at publication time and that no one has been talking about dual-licensing. The README still says AGPL-3.0; assume that's current.
- [ ] **Brand convention in print.** The repo uses "Wari" throughout; the rebrand to "和力 · Hé Lì / Heli" is brand-layer. Decide which name leads in the post title and which is the parenthetical. Recommendation: lead with **和力 · Heli (formerly Wari)** for new readers, and use "Heli" consistently in body text — easier to type and read across audiences than the hanzi.
- [ ] **"oci2wasm" name.** The repo references this Phase 2 tool by that name. If a competing tool ships with that name by publication time, rename to `wari-oci2wasm` or similar before the post commits a brand to the name.
- [ ] **Single-hart vs. SMP.** Today INV-1 (single-hart kernel) is load-bearing across many `unsafe` blocks. The post should not promise SMP. Phase 2+ revisits.
- [ ] **Density claim "10k–50k tenants/board"** is a *target*, not a measurement. There is no Tier-1 stress test on disk yet. Phrase as "designed for" or "target."
- [ ] **Cold-start claim "<10 ms"** is a target. wasmi is an interpreter; the actual measured cold start on Phase 0 was "<50 ms" per the Phase 0 exit criteria. Phrase carefully.
- [ ] **External audit citation.** The audit cadence in `docs/security-model.md` says external review at Phase 3 gate. The post should not imply audits have been completed yet.
- [ ] **Cite the prior 2026-04-11 blog post explicitly** as the continuity ancestor — readers should see this as part of a series, not a fresh project.
- [ ] **Confirm the manifesto's Spanish tagline.** "Soberanía tecnológica, tierra y libertad." Keep it in Spanish in print; that's deliberate per `docs/manifesto.md` §VI.
- [ ] **Re-read `docs/prior-art.md`** before any "what we reject" claim hits the page. Specifically: V8/JavaScript, OCI compatibility as architecture, gVisor-style syscall shims, Intel SGX lineage, WASIX. Don't invent new rejections in the blog post that the doc doesn't already commit to.
- [ ] **Confirm the convergence-thesis framing in `docs/manifesto.md` is the canonical text.** If Gustavo has been editing the manifesto, the dossier may be slightly stale. Cross-check the three principles (Ayni / Tequio / Hé) and the architectural-primitive mapping (capabilities / two-tier sandbox / explicit IPC) before reusing the language verbatim.

---

*End of dossier. All paths referenced are absolute under `/Users/goose/projects/wari/`. The dossier is ~7,000 words of research base; the blog post should compress this to ~2,500–3,500 words with Gustavo's voice on top. The single highest-leverage edit before publication is the §7 "honesty matrix" — every claim in the post should be traceable to a row there.*
