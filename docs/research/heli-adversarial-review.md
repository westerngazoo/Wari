These SAFETY comments do NOT cite INV-N — they're informal. R1 is technically violated in the driver crate. (R1 lives in CLAUDE.md "the kernel"; arguably the driver is not "kernel"; but the dossier elides this.)

I have enough. Writing the dossier now.

---

# Adversarial Review: 和力 · Heli (Wari) — Due-Diligence Memo

Prepared from `/Users/goose/projects/wari` at commit `b6c0a43`, build 121, May 2026. Scope: the dossier at `docs/research/heli-blog-dossier.md`, the on-disk artifacts it references, the merged PR history, and the project's market thesis.

---

## 1. Verdict and one-page summary

**Recommendation: defer.** Heli is an unusually well-disciplined solo-founder kernel-engineering exercise with a real silicon photograph attached and a real signed-WASM driver pipeline. It is not yet a product, an institution, or a credible bet for sovereign-grade procurement. The convergence-thesis framing is competently written but does not survive scrutiny as a *defensible market moat*; the technical work is more interesting than the marketing around it.

Three lines:

1. *Engineering*: small, disciplined, with internally consistent invariants — but smaller than the dossier implies it is correct about (227 unsafe blocks in the net driver alone, INV-1 single-hart load-bearing across the cap subsystem, wasmi-in-S-mode making "Layer 2 contains the escape" partly cosmetic).
2. *Viability*: one developer, AGPL-3.0, no commercial backer, no audit, no customer; the comparable list of completed-or-died microkernel projects suggests Heli needs ~10–30 person-years of additional investment to reach what the dossier describes as Phase 3, and there is no funding mechanism on the page.
3. *Market*: the LATAM-sovereignty + Chinese-long-cycle-capital + civilizational-convergence pitch is internally elegant but exhibits the classic founder pattern of three audiences each compelling alone and not one of which is buying yet. The convergence thesis is the strongest *narrative* asset and the weakest *commercial* one.

**Page summary.**

The disk evidence:

- Kernel: **8,784 Rust LOC + 197 asm LOC** (the dossier admits this honestly; the README still says "5–10 KLOC"). 47% of the kernel is the cap subsystem.
- wasmi 0.32.3 is in the TCB, ed25519-dalek 2.2.0 is in the TCB; full transitive dep graph for the Tier-0 build is **61 crates** including curve25519-dalek, fiat-crypto, sha2, subtle, zerocopy, hashbrown, spin, smallvec, libm, paste, syn, quote, proc-macro2. "Only third-party in TCB is wasmi" (dossier §2.1) is an overstatement.
- Net driver: **2,949 lines of Rust + ~30 KLOC of smoltcp** running in Tier-2 wasm32 sandbox. The driver contains **227 unsafe blocks**, none of which cite an INV-N (R1 is honored in `kernel/src/`, but not in `drivers/`).
- Cap system has 7 Kani harnesses on the pure-logic mint primitive. Revocation, IPC cap transfer, generation-counter monotonicity across mint+delete state machines, and slot-bounds-on-the-fast-path are *not* proved (the proofs.rs docstring admits this).
- The ed25519 `ACCEPTED_PUBKEY` is a 32-byte constant baked into `kernel/src/runtime/sign.rs:68`. The corresponding private key is **committed in-tree** at `scripts/dev-keys/wari-dev.ed25519.sec` with a "NOT FOR PRODUCTION" README. There is no key-rotation, key-custody, or key-recovery story on disk. The docstring for `ACCEPTED_PUBKEY` still says "TODO replace this [0u8; 32] placeholder" — it isn't a placeholder anymore, but the comment is stale.
- Fuzz harness pins `wasmi = "=1.0.9"`, the kernel pins `wasmi = "=0.32.3"`. Different major version. The fuzz target is not exercising the same code the kernel runs.
- Adversarial cap-system tests enumerated in `cap-system-design.md` §8.2 (`tier1_forge_cap.rs` et al.) **do not exist on disk** — confirmed by `ls tests/security/tests/`. The dossier notes this in §3.9 but the merge-gate language in CLAUDE.md is therefore aspirational.
- No external audit. Phase 0 audit document (`docs/audits/phase-0.md`) is **pending sign-off** per its own §Sign-off section, by Gustavo on Gustavo's own code with Claude as collaborator. That is not what an external auditor means.

The dossier acknowledges most of these points individually (it is honest) but combines them into claims that read stronger than each component supports. This memo's job is to surface the combinations.

---

## 2. Axis-by-axis findings

### Axis 1 — Engineering soundness

**INV-1 single-hart is more load-bearing than the security-model doc admits.** Of the 91 `SAFETY:` comments in `kernel/src/`, a hand count shows ~38 cite INV-1 directly or via INV-8/INV-14 which inherit from INV-1. INV-1 covers the cap storage (`cap/storage.rs:78,89`: `cspaces()` and `object_pools()` both return `&'static mut`), the IRQ binding table (`mmio/plic.rs:231,246`), the Tier-2 driver singletons (`runtime/tier2_uart.rs`, `runtime/tier2_net.rs`), the bump allocator (`runtime/heap.rs`), the scheduler process table (`sched/mod.rs:69` `static mut PROCESSES`). When SMP lands — and the roadmap puts SMP in Phase 2+, three phases out — every one of these sites needs a fresh proof. The Phase-1b cap-system PRs shipped with `unsafe` blocks that depend on INV-1; replacing INV-1 is a rewrite of the storage layer, not a tweak.

This is not "an invariant we'll revisit." The cap subsystem's storage model *is* `static mut [CSpace; MAX_PROCS]` with read accessors returning `&'static mut`. That pattern does not survive SMP without either per-hart CSpaces (changes the ABI) or kernel locks (changes the IPC latency story). The dossier mentions SMP only as a Phase 2+ deferral; this reviewer would weight it heavier — INV-1 is the *foundational* invariant, not a Phase-0 simplification.

**wasmi-in-Tier-0 is a single point of failure the security model understates.** `docs/security-model.md` §"Three layers" claims a wasmi validator bug is "contained" by Layer 2 (the MMU). This is partly true and partly cosmetic. The wasmi interpreter runs *inside the kernel address space at S-mode*. A wasmi memory-safety bug that lets a Tier-1 module corrupt wasmi's interpreter state directly corrupts kernel memory. The MMU does not save you because wasmi *is* the kernel from the page-table's perspective. The relevant escape primitive is "type-confusion in wasmi's instruction decoder leading to OOB write in the host Rust heap," and that gives you everything immediately.

The right honest framing is: Layer 1 (wasmi) protects against the *common* case (malformed bytecode, out-of-bounds linear-memory access caught by wasmi's bounds checks). Layer 2 (MMU) protects against the *rare* case where wasmi's bounds checks fail but the bug doesn't corrupt wasmi's own state. For a soundness bug in wasmi's host-side code, both layers are the same layer. wasmi has had soundness bugs historically; pinning v0.32.3 and not fuzzing it (the fuzz harness uses 1.0.9!) leaves the Tier-0 TCB exposed.

**smoltcp inside Tier-2 is a separate problem from what the dossier claims.** Dossier §3.4: "A TCP CVE in smoltcp affects one tenant's traffic, contained by the driver's WASM sandbox + the cap layer." The cap layer protects against driver→other-driver and driver→Tier-1 escalation. It does NOT protect against the network attacker who is the entire reason TCP CVEs matter. A remote attacker crafts an IP packet that hits smoltcp's parser, exploits a buffer-handling bug, and now controls the net driver's wasm linear memory. The driver has caps to talk to the NIC and to `wari_lin_mem_base` (which gives it access to its own linear memory). The blast radius is *every packet for every tenant*: there is one smoltcp instance, one NIC driver, one MAC. The dossier's claim that this contains-by-design is true for tenant-A-attacks-tenant-B-via-driver-bug, false for remote-attacker-takes-the-network. smoltcp's CVE history is short (it is a small codebase) but it is not zero, and it has not been the subject of dedicated fuzzing comparable to BoringSSL.

**The build-tag stale-driver guard is the right fix to the wrong abstraction.** The dossier presents `WARI-DRV-BUILD-TAG-N` as evidence of build discipline. It is also evidence that the kernel-embeds-signed-wasm-blob design has a correctness failure mode that the team has already been bitten by (builds 107–114 silently shipped a stale driver). The patch is to add a build-time grep. The structural problem — that `cargo build` doesn't know about the upstream wasm32 crate — is unfixed; the make-must-be-the-entry-point invariant is a manual convention that any new contributor can violate. seL4 doesn't have this failure mode because seL4 doesn't have this architecture. It's a design tax of the "drivers as wasm blobs included by `include_bytes!`" choice that the dossier markets as a security advantage.

**`static mut` proliferation in `cap/` is a code smell the dossier doesn't address.** `kernel/src/cap/storage.rs` documents two `static mut`s; `kernel/src/sched/mod.rs:69` adds a third (`PROCESSES`); `kernel/src/runtime/heap.rs` adds two more (`HEAP_CURSOR`, `HEAP_END`); `kernel/src/runtime/tier2_uart.rs:97` and `tier2_net.rs:73` each add a singleton. The pattern is sound under INV-1 + INV-8; it is also exactly the pattern that Rust 2024 edition is moving to require `addr_of_mut!()` around for a reason. As more subsystems join, the surface area of "things that depend on INV-1" grows in lockstep. Tock OS and Hubris both made different choices here (cell-based interior mutability with per-task contexts) precisely to avoid this. The choice is defensible at Phase 1b; it is *escalating debt* relative to a SMP-able future.

**Cherry-pick from goose-os is a smaller inheritance than implied.** The dossier and CLAUDE.md describe a cherry-pick discipline. On inspection: `wari-mem/src/page_alloc.rs` and `wari-mem/src/page_table.rs` (1,275 LOC together) appear to be the inherited pure-logic core; `page_table.rs` is unsafe-free, `page_alloc.rs` has 6 unsafe blocks. The validator pattern and INV-N framework are documentary. The capability subsystem (4,128 LOC) is new for Wari. The cherry-pick is real but small. Calling Wari "the WASM-native rewrite" of goose-os is accurate; calling it "cherry-picked from a working microkernel" implies more inheritance than 1,275 LOC of pure logic + framework conventions.

**Audit-exempt crate list (`docs/invariants.md` §"Non-contributing crates") is one row long** — `wari-abi`. The actual host-testable surface (page_table.rs, validate.rs, the cap pure-logic) is larger than the exemption list reflects; the exemption list reads like it was filled in once and not maintained. Minor, but symptomatic.

**Verdict: Acceptable.** The engineering is real, careful, and visibly improving. The dossier overstates how much of the trust story is *currently* enforced versus *aspirational*. The single-hart-everywhere assumption is the biggest unsaid bet.

---

### Axis 2 — Viability (does this project ship)

**Personnel-years prior art.** Comparable verified-or-near-verified microkernels:

| Project | Personnel-years to verified production | Funding |
|---|---|---|
| seL4 (NICTA/Data61, 2004→2009 verified) | ~25 person-years to first verified release; ongoing | Australian govt + DARPA + commercial spinout (Proofcraft) |
| ATMO (Mars Research Group, 2023→SOSP 2025) | ~2 person-years for Verus proof; built on Hyperkernel/seL4 precedent | Academic lab + grants |
| Hubris (Oxide, 2020→2023 production in shipping product) | ~5–10 person-years; small team | Oxide Computer Co. ($90M+ raised) |
| Tock OS (Stanford, 2015→production in signal hardware) | ~10+ person-years across multi-PI lab | Academic + Google + others |
| RedLeaf (UCI, 2017→SOSP 2020) | ~4–5 person-years for the paper; project subsequently went quiet | Academic |
| Redox OS (2015→ongoing, not production) | ~10+ person-years volunteer; still not "shipped" as a server OS | Donations / volunteer |
| Theseus (Yale/RPI, 2017→ongoing research) | ~3–5 person-years per PhD cycle; research-only | Academic |
| HelenOS (2004→ongoing, not production) | ~50+ person-years over 20 years; still hobbyist-grade | Volunteer |
| Singularity (MSR, 2003→2008 cancelled) | ~25+ person-years; killed for business reasons despite tech soundness | Microsoft Research |
| Genode (2008→ongoing, niche commercial) | ~50+ person-years; commercial backer (Genode Labs) | Commercial + grants |
| MirageOS (Cambridge, 2011→ongoing, niche) | ~20+ person-years; partially commercial (Docker/Tarides) | Academic + Tarides commercial |
| Firecracker (AWS, 2018→production in Lambda) | ~10+ person-years to first production; ongoing investment | AWS internal |

Pattern: serious OS projects that reach production all have either (a) hyperscaler funding, (b) academic-lab cover, or (c) a commercial entity behind them. Heli has none of these on disk. Volunteer-only projects (HelenOS, Redox) either remain hobbyist-grade indefinitely or never reach the load-bearing-trust threshold.

Heli's current visible team is one person (Gustavo Delgadillo) with LLM collaboration. Build 121 in May 2026. Phase 1c (network driver bring-up) is still mid-flight on a single PHY-delay bug as of `STATE-OF-PLAY.md` 2026-05-14. To reach Phase 3 (CoVE + GAPU + external audit + multi-board clustering + per-module formal verification) from here requires somewhere between **15 and 40 person-years** of additional engineering, depending on how much of the formal verification track is academic-collaboration-dependent. There is no plan on disk for funding any of that.

**Bus factor is 1.** No commit shows a second engineer landing code. The PR review loop ("Gustavo reviews locally → Gustavo tests locally → Gustavo merges; Claude never merges") is a discipline for an LLM-assisted solo developer, not a multi-person team. If Gustavo stops, the project stops; there is no inheritance plan.

**AGPL-3.0 financial sustainability.** AGPL §13 is the right *defensive* license for the "built to be shared, not rented" pitch — it prevents AWS from running Heli as a managed service without source disclosure. It is also the *worst* license for attracting Chinese sovereign capital (China's enterprise software market has historically preferred Apache-2.0 / BSD; AGPL is generally regarded as toxic for proprietary integration with state-aligned tech stacks like Inspur and Phytium), the worst license for a commercial backer (no SaaS path without forcing source disclosure on customer-side modifications), and a friction point for LATAM ministry procurement (most ministry IT contracts assume the vendor is responsible for support and uptime; AGPL doesn't change that but doesn't help with it either). The dossier's framing in §4.3 that "AGPL is the license that defeats hyperscaler enclosure" is true; the unmentioned cost is that it also defeats most realistic revenue models. Heli's commercial story is therefore implicit donation/grant/foundation, which is the hardest sustainable model in software (cf. OpenSSL's pre-Heartbleed funding crisis as the cautionary case).

**Comparison to actual completed sovereign-OS efforts.** China's Kylin OS (a Linux/Ubuntu fork with state procurement, ~20+ years of dev) is a *Linux fork* with state contracts. Russia's Astra Linux is also a Linux fork. Cuba's Nova is Linux. The actual market for sovereign-cloud OS is "Linux variant with local support contract." Heli is betting on a category that does not exist as a procurement line item: "non-Linux WASM-native sovereign OS." The closest commercial existence proof is Oxide (Hubris under Helios under Illumos, ~$90M raised, still mostly hardware revenue not OS revenue). seL4's commercial spin-out, Proofcraft / DornerWorks, exists but as a consulting business, not a product line. There is no example of a *non-Linux* kernel succeeding in sovereign procurement in the last 15 years.

**Verdict: Weak.** The engineering trajectory is plausible at the current rate; the *institutional* trajectory to "production-grade sovereign infrastructure for hospitals in Oaxaca" is not visible. The dossier conflates "kernel that boots on real silicon" (true) with "kernel that an institution can stake citizen records on" (a 10+ person-year, $5–50M gap).

---

### Axis 3 — Security (steelman the realistic attack surface)

**The Tier-1 → wasmi → Tier-0 trust gradient is the primary attack surface and the dossier handles it lightly.** The realistic attack chain for a Tier-1 escape:

1. Find a soundness bug in wasmi 0.32.3 (a pure interpreter, ~30 KLOC, with `unsafe` in its core hot path for native-host-call dispatch).
2. Trigger it via a crafted wasm module that passes the validator but exploits a decoder edge case.
3. Corrupt the host-side Rust state of the wasmi instance — which lives in the kernel's S-mode address space, identity-mapped, RW.
4. Pivot to the kernel's `static mut` capability storage (always in scope from the wasmi instance, all under INV-1+INV-8).
5. Mint arbitrary capabilities. Game over.

The MMU does not help at step 3 because both the wasmi runtime state and the kernel `static mut`s are in the same address space (S-mode). The security model doc papers over this with "Layer 2 catches escapes" — but Layer 2 catches a *Tier-1-issued raw-pointer dereference outside the wasm linear memory*, not a *wasmi-soundness-bug-induced corruption of host memory*. These are different threats.

**Mitigation requires either (a) running wasmi in a separate address space from the rest of the kernel (which Heli does not do and is non-trivial), (b) formal verification of wasmi (Phase 4, speculative), or (c) treating wasmi as part of the TCB and saying so plainly.** The dossier and security-model.md should adopt (c) until (a) or (b) materializes.

**ed25519 signature gate.** `ACCEPTED_PUBKEY` is a single 32-byte constant compiled into the kernel binary. The dev secret key is checked into the repo. There is no:

- key-rotation mechanism (changing the trust root requires a kernel re-flash);
- key-revocation list (a compromised signing key cannot be retired without a re-build);
- per-tenant or per-environment key (dev / staging / prod share the same trust root in the current design);
- HSM or hardware-token path on disk (the README at `scripts/dev-keys/README.md` says "decided at the time" for Phase 1);
- audit trail of which key signed which blob (the envelope contains the pubkey, but there's no manifest of "this blob signed by which authority on which date for which target").

The dossier's claim (§3.3) that "drivers are ed25519-signed" is true. The *security architecture* around that signing — what auditors call PKI hygiene — is a Phase-1-or-later TBD. A procurement-side reviewer asking "what is the key-custody plan" gets no answer from the current repo.

The signing private key being committed in-tree is fine for dev, common pattern. The structural problem is that the README admits the production key path is "decided at the time" — production-grade key custody is a sub-project of its own (HSM integration, signing-ceremony procedures, multi-party offline sig, transparency log) that hasn't started and isn't in any phase plan.

**smoltcp CVE exposure.** smoltcp 0.11 is a small no_std stack with a small but nonzero advisory history (search RustSec/`cargo audit` advisories). The dossier's framing that smoltcp-in-Tier-2 is *better* than Linux-TCP-in-kernel is correct for the *blast-radius* dimension. It is worse for the *patch-velocity* dimension: Linux's TCP stack gets a CVE fix within hours, distros push within days. Heli's process is: smoltcp fixes upstream → Wari bumps Cargo.toml → re-signs the driver wasm → flashes to every deployed VF2 → operator runs `wari upgrade`. There is no OTA infrastructure, no signed-update transport (the `wari upgrade` script is a `git pull`). For a sovereignty buyer, "what's the patch cadence" is a contractual question; Heli's answer today is "manual, by the operator."

**Side channels and timing oracles.** wasmi is a tree-walking-ish interpreter with no constant-time discipline. Tenants sharing the interpreter on the same hart can observe each other's instruction-mix via cache-timing. This is not unique to Heli (it is the price of shared-runtime density, same problem Cloudflare Workers and Fastly have), but Cloudflare/Fastly mitigate via heavy invariants on what kinds of secrets are allowed in the runtime context and operate at scale that diffuses any single observation. Heli's "10k–50k tenants/board" target on a 4-core 1.5 GHz JH7110 will have observable cross-tenant cache contention as a baseline. The dossier doesn't mention this; it should.

**Cap-system trust surface quantification.** The dossier (§3.5) admits Kani proves *only the pure-logic mint primitive*. Specifically what is *not* proved:

| Trust surface | Proved? | Where? |
|---|---|---|
| Mint rights monotonicity | Yes | `cap/proofs.rs::derive_preserves_rights_monotonicity` |
| Mint reserved-bit rejection | Yes | `derive_rejects_reserved_rights_bits` |
| Mint kind/pool preservation | Yes | `derive_preserves_kind_and_pool_index` |
| CapId encoding round-trip | Yes | `capid_round_trips` |
| Revocation cascade termination | No | `cap/revoke.rs`, no proof |
| Revocation cascade soundness (no leaked descendants) | No | — |
| Generation counter monotonicity across mint+delete | Partially (cspace unit tests, not Kani) | — |
| IPC cap transfer (badge preservation, parent-id propagation) | No | — |
| Slot-bounds on the fast path | No | — |
| Tier-shape compatibility (INV-19) | "Structurally a no-op today" per the invariant doc | — |
| Cross-process revoke under concurrent mint | N/A (INV-1 makes it impossible today) | — |

So of the ~11 trust surfaces in the cap subsystem, **4 are Kani-proved at the pure-logic level, 7 are unit-tested or asserted by code review.** The dossier's compressed framing "Kani-proved on the critical mints" is technically accurate; "the capability system is Kani-proved" — which is what a casual reader takes away — is overstated. The proofs cover the easiest cases; the hard cases (revoke cascade, IPC transfer) are the unproved ones.

**"No ELF in customer ABI" enforcement.** Slot 16 (`SYS_SPAWN_ELF` retired) is enforced *by absence*, not by an active check. A future contributor adding a syscall handler for slot 16 would bypass nothing — the rule lives in CLAUDE.md and a comment in `abi-shared/src/lib.rs`. There is no test that fails when slot 16 is wired up. The dossier could harden this by adding a compile-time test that asserts no handler is registered for slot 16, or a clippy lint. Today it is a strong cultural rule with no mechanical enforcement.

**Phase 3 CoVE spec stability.** RISC-V CoVE was ratified in 2024. Silicon implementations are roadmap-2026-27 from JH7110-class vendors. As of the conversation date (mid-2026), production silicon with CoVE is not yet broadly shipping. Heli's Phase 3 plan depends on a hardware feature that has not yet manifested at the volume Heli is targeting. If CoVE silicon arrives late, slips, or arrives with implementation bugs (Intel TDX had several in its first generation), the entire Phase 3 security story slips with it. The dossier acknowledges this in §1's "honestly speculative" line; the *blog post* will need to be careful not to imply CoVE is shipping.

**Build supply chain.** `Cargo.lock` is committed (R8 ✓). The toolchain is pinned to Rust 1.95.0 (R8 ✓). The actual TCB-included dependency graph includes 61 crates per `Cargo.lock`. Notable transitive crates that compile into Tier-0 via ed25519-dalek and wasmi: `curve25519-dalek`, `curve25519-dalek-derive` (proc-macro at build time), `fiat-crypto`, `sha2`, `subtle`, `zerocopy`, `zerocopy-derive` (proc-macro), `syn`, `quote`, `proc-macro2`. Each of these is a supply-chain hop. `proc-macro2` and `syn` are not in the runtime TCB but execute arbitrary code at build time as part of any derive — a malicious crates.io publish of a patch version of `syn` would compromise Heli's reproducible build. The mitigation is "we pin to exact versions in Cargo.lock"; the gap is that there is no `cargo vet` or `cargo crev` audit-trust attestation chain, no `cargo audit` in CI evidence on disk. R8 is honored at the "lockfile committed" level, not at the "verified-build-from-attested-sources" level a Sovereign procurement standard would require.

**Verdict: Weak.** The defenses that exist are real and the discipline is unusual for a solo project. The set of unaddressed seams (wasmi-in-S-mode, smoltcp patch-velocity, key custody, cap-system proof coverage, build supply chain attestation, side-channels) is large enough that the "three layers of isolation" framing in the security model is closer to *one layer plus two future layers* than the doc admits.

---

### Axis 4 — Architecture (independent of marketing)

**Is two-tier WASM novel?** Adjacent prior art:

- *Tock OS* (Levy et al., SOSP 2017) — Rust kernel + Rust capsules (drivers) + Rust processes; the capsules-as-language-isolated-drivers pattern is the Tock contribution. Tock is in production in security-critical hardware (Signal Capsule). Heli's contribution over Tock is *WASM instead of Rust source* for the driver — auditable as a binary blob with a signature gate, language-agnostic for the driver author. That is a real delta.
- *Singularity OS* (MSR, 2003–08) — managed-code OS with language-enforced isolation (C# SIPs); the conceptual ancestor.
- *RedLeaf* (Narayanan et al., SOSP 2020) — Rust "domains" with kernel-mediated boundaries; closest academic sibling per the dossier.
- *Hubris* (Oxide) — static task set, Rust microkernel; production at Oxide; not WASM.

The two-tier-WASM contribution over this set is *real but incremental*: it is "Singularity with WASM instead of CLR, RISC-V instead of x86, AGPL instead of Microsoft-proprietary." Calling it "the architecture nobody else ships" (dossier §8 hook 5) overclaims; "the production OS shipping the Singularity-shape design on open ISA" is more honest.

**Is "drivers as signed WASM" novel?** Signed kernel modules exist (Linux module signing, Windows driver signing, Apple's kext signing). *WASM-sandboxed* signed kernel modules are essentially unique to Heli among hobbyist/research projects this reviewer knows of. The structural delta from Linux is real: a Linux signed module runs at ring 0 with full hardware access; a Heli signed driver runs in wasmi with capability-gated MMIO. That is a legitimate architectural advantage *if* wasmi is trustworthy (see Axis 3).

**Performance reality.** wasmi 0.32.3 is a pure interpreter, no JIT. On a JH7110 U74 at 1.5 GHz, realistic single-thread interpreter throughput for tight-loop Rust-compiled-to-wasm is ~0.05–0.2x native (interpreter overhead is dominant). For a TCP echo server: the request-response cycle goes through Tier-1 wasmi → host fn dispatch → Tier-2 wasmi (smoltcp interp) → MMIO → reverse. Estimated cold throughput at Phase 2: order **100s of req/sec per core**, possibly low single-digit thousands with hot caches. Compare Cloudflare Workers' V8-JIT'd millions/sec per core. The dossier's "10k–50k tenants per board" is a *density* target (mostly-idle tenants), which is plausible because wasm linear-memory per idle tenant can be small. The *throughput* number is conspicuously absent. For sovereign cloud use cases that actually need to serve traffic (a health ministry's patient portal under load), wasmi-interpreter throughput is not viable without JIT, and the dossier defers JIT to Phase 2+ with no committed implementation.

**Phase 3 GAPU FPGA.** Dossier §5 admits this is extrapolation. The repo contains *zero design doc* for GAPU at the level of `docs/net-driver-design.md`. The Phase-3 GAPU strawman in the dossier is a thought experiment, not a plan. A "GAPU FPGA over PCIe" Tier-2 driver requires: PCIe stack (not in Heli; smoltcp doesn't help, this is a different subsystem), DMA-coherent memory management (not in Heli; the Phase 1c memory model is single-VA-space identity-mapped), IRQ-routing for PCIe MSI-X (not in Heli; PLIC handles SoC IRQs only), and an FPGA bitstream signing/loading pipeline (does not exist). Each of those is multi-month work. Calling GAPU "designed Phase 3" in the dossier honesty matrix is generous; it is *named*, not designed.

**Phase 4 MMU-free custom silicon.** A 5–14 nm RISC-V tapeout is **$10–50M and 18–36 months**, plus 6–12 months of post-silicon bring-up. For comparison, Oxide's Helios stack rides on commodity AMD silicon precisely to avoid this; even Apple's silicon team takes years per generation with billion-dollar budgets. Putting "Tier-0 frozen-image spec → SoC RTL tapeout → kernel-in-ROM" on a roadmap as Phase 4 milestones, for a one-developer AGPL project, is *aspirational* in the literal sense (it is an aspiration). The dossier honestly labels this "Speculative Phase 4+ option." Outside the dossier, the README's "designed for formal verification" copy and the cap-system-design.md's "Verus track Phase 4" framing flirt with treating Phase 4 as a plan. It is not a plan; it is a north star.

**Multi-tenant TCP architecture is genuinely under-specified.** smoltcp runs *in one driver instance*. With one MAC on the board, the architecture must multiplex 10k–50k Tier-1 tenants across one TCP stack, which means either (a) per-tenant smoltcp instance in per-tenant Tier-2 driver instances (would need per-tenant signed driver loads, defeating the density model), (b) socket-multiplexing in a single smoltcp instance with per-tenant cap-gated socket handles (the design glimpsed in `cap/syscall.rs::net_socket_*_impl`), or (c) a different design entirely (per-tenant linux-netns-equivalent). The current code commits to (b) at the syscall level but `docs/net-driver-design.md` doesn't yet specify how 10k tenants share one smoltcp instance — connection-table capacity, per-tenant connection limits, fairness, head-of-line blocking, port-allocation collision. These are *socket-layer* problems with known solutions but Heli has not chosen one and committed it to disk.

**Verdict: Acceptable** for the engineering done so far, **Weak** for what the architecture commits to beyond Phase 1c. The dossier's "double sandbox by construction" framing in §2.5 is the strongest defensible architectural claim; "Phase 3 GAPU" is the weakest.

---

### Axis 5 — Engineering principles (R1–R8, four principles, PR template)

**R1 (every unsafe cites INV-N) — honored in the kernel; not honored in the driver.** Spot-checking 10 unsafe sites in `kernel/src/`: every one has a `// SAFETY:` comment citing an INV-N. Discipline confirmed. Spot-checking 10 unsafe sites in `drivers/net/src/lib.rs` (the file with **227 unsafe blocks**): the SAFETY comments are informal English ("caller passes a valid offset within our lin-mem; wasmi traps on OOB"), **none cite an INV-N**. The CLAUDE.md R1 text says "every `unsafe` block ... in the kernel" — strict reading exempts the driver. The dossier however frames the cap-and-driver story as one trust system, and the driver crate has more unsafe than the kernel proper. The discipline is incomplete relative to the marketing.

**R5 (no panics in syscall paths).** Grep finds `.unwrap()`/`.expect()` in non-test paths:

- `kernel/src/sched/mod.rs:166,178,201`: three `.unwrap()` on `table[proc_id as usize].as_mut()` / `.as_ref()`. These are in the scheduler, which is on the syscall path indirectly (a syscall handler runs in the context of a process). The unwraps assume the process table entry is populated; if a syscall handler is invoked with a stale `proc_id`, the kernel panics. This is a literal R5 violation in the syscall hot path.
- `kernel/src/runtime/tier2_net.rs`: six `.expect("TIER2_NET ref always valid (static)")` calls on accessors used by net syscalls. Same R5 concern.
- `kernel/src/runtime/mod.rs:169,171`: `.unwrap_or(-99)` — that's the safe form, not a violation.
- `kernel/src/cap/syscall.rs:1011`: `.unwrap_or(-1)` — safe form.

R5 is *mostly* honored. The scheduler and tier2_net accessors are the exceptions, and they're load-bearing on INV-1+INV-8+INV-14 to never trip. They will trip if any of those invariants break — which is exactly the case formal verification is supposed to protect against.

**R8 (reproducible builds).** `Cargo.lock` is committed; `rust-toolchain.toml` pins 1.95.0. This is the standard Cargo definition of "reproducible." The stronger sense — bit-for-bit identical binary output across two clean clones on two machines — requires also pinning the linker (lld variant + version), `cargo`'s embedding of timestamps in debug info, and the host C toolchain (for any C in the dep tree, none in Heli's TCB strictly but possibly in `proc-macro2`'s build script chain). There is no `diffoscope`-tested-reproducible attestation on disk. R8 is honored at the *intent* level, not at the *attested-build* level a sovereign-procurement reviewer would expect.

**Tactical-vs-structural change discipline.** Reading the last 10 merged PRs: PR templates are followed scrupulously when the PR is small (PR #22 docs reconcile is exemplary). PR #20 ("Phase 1b/net 5a smoltcp device") and PR #19 ("Phase 1b/net 4c virtqueues") are larger and the templates are present but more compressed. PR #17 (net MMIO host fns) does not strictly follow the "## Why / ## How / ## Invariants affected / ## Security considerations" canonical headers — it uses a less-structured ASCII-divider style. So: template compliance is **about 70%**; the security-considerations section in PR #17 reduces to "Behavioural change at runtime: NONE" which is short of the "thoughtful, not boilerplate" bar in CLAUDE.md.

For a solo developer, this is fine; for a reviewer looking at "is the PR discipline real," it is *aspirationally enforced* not *mechanically enforced* (no PR-template CI check, no merge-blocker on missing sections).

**Prior-art citation discipline.** Spot-checking `docs/prior-art.md`:
- seL4 caps → confirmed cited in `kernel/src/cap/types.rs` docstring (cspace.rs cites cap-system-design.md which cites Klein et al.). ✓
- Fastly WASM-as-boundary → cited in design docs, not in code comments. ✓-ish
- Cloudflare shared-runtime density → cited in `docs/prior-art.md`, in the docstring of `runtime/host_fns.rs`? Did not verify in detail. The doc is comprehensive.
- The rejected list (V8, OCI, gVisor, WASIX, SGX) is sound and well-argued.

This is genuinely better than most projects this size.

**"Every architectural pattern cites prior art" claim.** Mostly true, especially in design docs. There are gaps: the YT8531C RGMII delay calibration logic in build 121 cites mainline VF2 device tree as prior art (correct) but the broader pattern of *driver in WASM with PHY register sequence* is novel-to-Heli and not framed that way. Minor.

**Verdict: Strong** for kernel discipline, **Acceptable** for driver discipline, **Weak** for PR template enforcement (manual, not mechanical). The R1 gap on driver unsafe sites is the biggest concrete-fix item: extend the invariants doc to cover driver-side unsafe with the same rigor, or move the discipline into a clippy lint that fires on `unsafe` without `INV-N` in the preceding comment.

---

### Axis 6 — Market (sovereignty / LATAM / Chinese capital / convergence thesis)

**LATAM public-sector procurement reality.** This reviewer cannot verify specific RFP-win statistics without external sources, but the structural priors:

- *Brazil's POSIX-compliant government computing initiative (SISP)* mandates open-source preference but in practice procurement defaults to Red Hat / SUSE / Canonical with local-integrator support contracts. The procurement officers are buying *support and accountability*, not source openness.
- *Mexico's federal IT procurement* under SFP guidelines: similar pattern, defaults to known commercial vendors with Mexican channel partners (think Softtek, Sonda).
- *Argentina, Colombia, Chile, Peru*: same pattern, with occasional notable exceptions (Peru's free-software law from 2005 had limited binding effect).
- No record this reviewer can verify of a non-Linux kernel winning a meaningful public-sector cloud-infrastructure RFP in LATAM in the last decade. Argentina's "Huayra GNU/Linux" is the most-cited example and is a Debian fork.

The structural problem: a LATAM ministry CTO buying infrastructure for a hospital wants (a) someone to call when it breaks at 3 AM, (b) someone whose contract obliges them to fix it, (c) a license that the legal team has seen before, (d) certifications (ISO 27001, FedRAMP-equivalent, local data-protection law compliance). Heli today provides *none of these*. The README's "Latin American institutions deserve infrastructure whose ownership, governance, and inspectability they can verify directly" is true; what they ALSO need — and what the dossier doesn't address — is a *vendor on the hook*.

The pitch line in §4.1 ("you buy the box, you own the box, you can read every line of code") assumes a procurement officer who has both the will and the institutional capacity to run their own OS internally. That officer mostly doesn't exist. The ones who do work for telcos and infrastructure providers (Telefónica, Claro, América Móvil), not ministries.

**Chinese long-cycle capital.** This reviewer's confidence here is *speculative on the reviewer's part*. The structural priors as best understood:

- China's "信创" (xìnchuàng) — Information Technology Application Innovation — initiative explicitly favors *domestic* hardware, OS, database, middleware. Loongson + UnionTech (UOS) + Phytium + Inspur is the canonical stack. A foreign-developed kernel under AGPL faces both a regulatory friction (is the foreign developer subject to US export controls? CFIUS? the BIS Entity List?) and a strategic-preference friction (Chinese sovereign infrastructure money flows to Chinese-led projects).
- AGPL-3.0 in China: enforced cautiously, but Chinese legal posture toward copyleft for state-aligned software is wary. Loongson, UOS, Kylin all primarily ship under Apache-2.0 or BSD or proprietary terms.
- "Family-office partner in Singapore, state-aligned VC in Shenzhen, Hong Kong PE fund with 10-year IRR horizon" (dossier §4.3) is a real audience but vanishingly thin for *non-Chinese-led* open-source infrastructure. The closest precedent is Tarides (MirageOS commercialization, French-led, took European structured grants more than Chinese capital). This reviewer is aware of no Western-led open-source infrastructure-kernel project that has raised meaningful Chinese sovereign-style capital in the last decade.
- The cultural-fluency pitch (Ayni / Tequio / Hé) is genuinely novel as packaging. Whether it converts to capital is unverified; this reviewer's prior is *low likelihood*. Capital follows track record and team, not narrative resonance.

**Convergence thesis as intellectual work.** The Ayni-Tequio-Hé framing is well-written. A skeptical anthropologist would object on three grounds:

1. *Conflation.* Ayni (Andean reciprocity in agricultural-pastoral household economy), tequio (Mesoamerican communal labor for shared infrastructure), and Confucian hé (Imperial statecraft's relational harmony) operate at very different scales and serve very different state-society contracts. Mapping all three to "every node has I/O obligations to the network" abstracts away most of what makes each interesting in its own context. The mapping is *evocative*, not *equivalent*.
2. *Appropriation risk.* The manifesto invokes "1,400-year-old validated social technology" (Wari Empire, 600–1000 CE) as load-bearing branding for a kernel project led by a Bolivian-Mexican founder, which is the strongest case for cultural legitimacy. The Confucian frame requires more care; using *和* as kernel marketing without explicit collaboration with Chinese partners is a position the manifesto adopts unilaterally. A Chinese reader sympathetic to the broader sovereign-tech project may nevertheless raise an eyebrow at the cooptation. The dossier flags this risk implicitly ("not anti-anything") but doesn't engage with it directly.
3. *Functional gap.* "Capability tokens = ayni" is metaphorically pretty. The actual cap-system mechanism is borrowed from seL4, which is borrowed from KeyKOS / EROS, which is descended from the 1960s capability-machine literature. The Andean source is *layered on top of* an existing CS lineage, not *generative of* the design. A reader who knows both Andean economic anthropology and CS capability theory will see two unrelated structures being labelled with each other's vocabulary. This is rhetoric, not architecture.

This is the most likely point of vulnerability when the convergence thesis hits a serious external audience. The kindest framing: "Heli draws inspiration from these traditions" — true, defensible, not over-claiming. The dossier's framing — "This isn't a metaphor laid on top of an unrelated codebase. The cap-derivation tree literally encodes 'who owes a debt downstream from whom'" — is the over-claiming version that an academic reviewer will flag.

**Competitor landscape.**

- *Inspur, Phytium, Loongson, Kylin* — the Chinese-sovereign-OS field. Heli is not a competitor here for state-tier procurement; they are the established stack. Heli could conceivably play the *RISC-V edge sovereign cloud* lane that none of these focus on, but with no Chinese state-aligned partner that lane has no procurement path.
- *Cumulus / SONIC* — open-source network OS for switches; Microsoft-backed SONIC is the production-credible one. Heli is not in the network-OS market.
- *seL4 commercial spinouts (Trustworthy Systems / Proofcraft / DornerWorks)* — these are the closest comparable-credibility offerings. They operate as consultancies/integrators, not product vendors. The market they've found is defense, aviation, automotive (high-assurance embedded). Heli's "10k–50k tenants per board" use case is *not* the seL4 spinouts' market; the seL4 market is small-tenancy, hard-real-time, certifiable-to-DO-178C.
- *Oxide (Hubris under Helios under Illumos)* — fully-integrated server vendor with $90M+ raised, no software-only revenue model, sells racks. Hubris itself is a competitor in the *narrow-purpose Rust microkernel* niche but not the *sovereign cloud* niche.

Heli's positioning is *between* these markets, claiming all of them and credibly serving none.

**AGPL contradiction with sovereignty buyers.** A LATAM ministry buying Heli would, under AGPL §13, be required to publish any in-house modifications to the operator interface. Ministry IT shops do customize. They typically do *not* want their customizations on the internet (e.g., custom audit-logging modules for the SAT in Mexico, or patient-database adapters for a state health system). AGPL forces a choice: don't customize, or publish the customization. Most ministries will choose "don't buy" rather than be on either horn. The dossier's pitch in §4.1 doesn't address this. The pitch in §4.3 frames AGPL as "defeats hyperscaler enclosure" — the unaddressed flip side is "deters sovereign customization."

**"Built to be shared, not rented."** This is a beautiful tagline for a manifesto. It is a poor tagline for a procurement-ready product. The sovereignty buyer wants the *opposite* — they want a rental contract with a vendor that takes responsibility. The contradiction is unaddressed in the dossier.

**Verdict: Weak** for LATAM, **Speculative on reviewer's part / leaning Weak** for Chinese capital, **Acceptable** for the cultural-narrative content as art, **Unsupported** for the cultural narrative as a load-bearing commercial moat.

---

### Axis 7 — Things the dossier elides or asserts without warrant

Line items, with rebuttals:

1. *"Three independent isolation layers per tenant from day one"* (§1, §3.1, §2.5). Layers 3a (PMP, Phase 1) and 3b (Zkn/Zks, Phase 2) and 3c (CoVE, Phase 3) are not Phase-1c — and Phase 1c isn't even fully shipped (network bring-up incomplete). Today the operative count is *two* (wasmi validator + Sv39 MMU), with wasmi-in-S-mode making Layer 2 partial protection against wasmi-soundness bugs (Axis 3). Honest framing: "two layers today, layered defense roadmap to four by Phase 3."

2. *"Capability system is seL4-shaped and Kani-proved on the critical mints, not retrofitted"* (§1). True for "Kani-proved on the mint primitive." The dossier elides that revocation cascade, IPC cap transfer, and slot-bounds-on-fast-path are NOT proved; the syscall layer is unit-tested but not Kani-proved. "Critical mints" is doing a lot of rhetorical work for "the easiest pure-logic functions, the hard ones still wait for Verus in Phase 4."

3. *"No comparable production OS treats drivers this way"* (§1, §3.3). True if you read "production OS" strictly to exclude Tock (which does treat drivers as language-isolated capsules, just in Rust source not WASM). The novelty is the *WASM* part, not the *signed isolated driver* part.

4. *"~9 KLOC kernel today"* (§1, §2.3). Add wari-mem (1,301), abi-shared (327), wasi (44) for the audit surface and you get ~10.5 KLOC of Wari-authored code in the trust path, plus 61 transitive deps in `Cargo.lock`. The dossier honestly recasts as "audit surface ~10.4 KLOC" in §2.3; the executive summary doesn't, and the README still says 5–10 KLOC. Marketing-vs-reality gap: ~40-100%.

5. *"smoltcp inside the net driver's WASM linear memory ... contained by the driver's WASM sandbox + the cap layer"* (§3.4). True for tenant-A-attacks-tenant-B-via-smoltcp-bug; false for remote-attacker-takes-the-network-via-smoltcp-bug, which is the threat that TCP CVEs actually concern. Containment story conflates two threat models.

6. *"Reproducible builds (R8)"* (§4.1). True at the Cargo-lockfile level; not attested at the diffoscope-bit-for-bit level a sovereignty buyer's audit would expect. Half-true.

7. *"Formal verification trajectory"* (§4.1). True as a roadmap commitment; Phase 4 in the dossier's own honesty matrix is labeled "Speculative." The line "the math is on a path to be checkable by your auditors" is the kind of sentence a senior infosec procurement officer reads as "this might happen in 5+ years," which is correctly stated but possibly not what casual readers parse.

8. *"Drivers are signed WASM, not binary blobs. ... No comparable production OS does this"* (§1). The driver is a signed blob; it is signed by a key checked into the repo (Phase 0 dev key) with no production-key-custody plan on disk. "Production OS" status is also doing rhetorical work — Heli does not yet have production tenants.

9. *"LATAM consumer market by 2030: $4.2T"* (README). This is the total LATAM consumer market across all goods and services. The addressable market for sovereign cloud infrastructure is *several orders of magnitude smaller*. Citing a TAM that includes groceries and clothing is the kind of pitch-deck inflation that loses credibility with a serious reviewer. The Mexican domestic market figure (130M) is "people in Mexico" — population, not market. These are not market-sizing figures; they are framing decoration.

10. *"1,400-year-old validated social technology"* (README, dossier §4.3). Wari Empire dated 600–1000 CE; Ayni as a continuous practice in Andean communities post-dates that, with substantial discontinuity through Spanish conquest, into the modern republican period, with current practices being syncretic. "1,400-year-old validated" is a *narrative* claim, not a historical one. Andean economic anthropologists would not endorse the phrase as written.

11. *"Phase 1b shipped, capabilities + scheduler + IPC + Tier-2 drivers"* (README phase table, dossier §7). Capabilities: shipped at the data-structure and pure-mint level, no full IPC cap-transfer proof yet. Scheduler: shipped at the "single Tier-1 instance + WFI loop" level, no preemptive multitenancy yet (the dossier elides this — Phase 0 exit criterion explicitly required preemption; Phase 1b appears to still be cooperative). IPC: Endpoint+Notification types exist; the sysnum surface (`SEND/RECEIVE/CALL/REPLY`) is reserved but the implementation is partial in the syscall.rs file. "Shipped" is generous for at least scheduler-and-IPC.

12. *"Net driver works at MMIO level on silicon"* (§1). `docs/STATE-OF-PLAY.md` 2026-05-14: "ARP replies are ~1/118 reliable." That is not "works." The dossier flags this in §9 as a pre-publication checklist item; the executive summary doesn't. The blog post can't claim Phase 1c shipping until the silicon test passes.

13. *"Density target 10k–50k tenants per board"* (§4.2). No measurement, no stress test on disk. Asserting against a 4-core 1.5 GHz JH7110 with 8 GB RAM: 50k tenants = 160 KB per tenant total memory budget including their wasm linear memory and runtime overhead. With wasmi 0.32.3's per-instance overhead (Store + Module + Memory + Engine state, ~100 KB minimum on no_std), this is *probably* infeasible at the 50k upper bound. Plausible at 1k–5k with small Tier-1 modules. The 10k–50k figure is *aspirational*; the dossier admits this in §9.

14. *"Cold start <10 ms target"* (§2.5). wasmi interpreter cold start for a non-trivial module includes wasm-bytes parse + validate + Module construction + Store construction + Instance construction. On a 1.5 GHz interpreter with no JIT, 10 ms is feasible for a small module, infeasible for a >10 KB module loading on first request. Phase 0 measured <50 ms per the exit criteria; the dossier's <10 ms is forward-looking.

15. *"Drivers as wasm makes the GPU driver shippable as wasm too"* (Phase 2/3 roadmap). A modern GPU driver is on the order of *millions of lines of code*. Even an embedded GPU driver is hundreds of thousands. Compiling all of that to wasm32 and running it in wasmi on a 1.5 GHz core is not viable. The Phase 2 "GPU driver over PCIe" line item glosses over a discontinuity in scale.

---

## 3. Claims that don't survive scrutiny — line items

| Dossier claim | Reality on disk | Severity |
|---|---|---|
| "5–10 KLOC kernel" (README) | 8,784 LOC + asm; ~10.5 KLOC audit surface including wari-mem; 61 transitive deps | Minor (the dossier admits this, README hasn't caught up) |
| "Only third-party dependency in TCB is wasmi 0.32.3" (dossier §2.1) | ed25519-dalek 2.2.0 + curve25519-dalek + sha2 + subtle + zerocopy + ... ≈ 61 crates in lockfile | Medium |
| "Three independent isolation layers from day one" (§1) | Two layers today (wasmi + MMU); 3a/3b/3c are Phase 1/2/3 | Medium |
| "Kani-proved on the critical mints" (§1) | 4-7 proofs on pure-logic mint primitive; revoke cascade, IPC cap transfer, slot-bounds-fast-path unproved | Medium |
| "Drivers as signed WASM" "ed25519-signed against ACCEPTED_PUBKEY" (§3.3) | True; key custody is Phase-0-dev-key-in-repo with no Phase-1 plan on disk | Medium |
| "Smoltcp ... contained by the driver's WASM sandbox + the cap layer" (§3.4) | Containment is real for cross-tenant; absent for remote-attacker-takes-network | High |
| "1,400-year-old validated social technology" (README) | Narrative claim with substantial historical discontinuity through colonial period | Minor for blog, Medium for serious reviewer |
| "LATAM consumer market by 2030: $4.2T" (README) | Total consumer spend, not addressable sovereign-cloud market; off by 3-5 orders of magnitude as TAM | High (credibility) |
| "Capability tokens = Ayni" (manifesto, dossier §2.6) | Andean source layered on top of a seL4-derived design, not generative of it | Minor for sympathetic readers, Medium for skeptics |
| "AGPL-3.0 defeats hyperscaler enclosure" (§4.3) | True; also deters sovereign-customer in-house customization and most known commercial revenue models | Medium |
| "Phase 1b shipped" includes scheduler + IPC (README phase table) | Scheduler is single-Tier-1+WFI; IPC sysnums reserved, implementation partial | Medium |
| "Network driver works on silicon" (§1) | ARP replies ~1/118 reliable per STATE-OF-PLAY.md 2026-05-14 | Medium (dossier acknowledges in §9) |
| "<10 ms cold start" (§2.5) | Phase 0 measurement was <50 ms; <10 ms is forward target | Minor |
| "10k–50k tenants per board" (§4.2) | Unmeasured; high end likely infeasible with wasmi 0.32 per-instance overhead | Minor (admitted) |
| "Phase 3 GAPU FPGA" "designed" (§7 honesty matrix) | Named in roadmap; no design doc on disk; PCIe / DMA / FPGA bitstream pipeline subsystems do not exist | Medium |
| "Phase 4 MMU-free custom silicon" (CLAUDE.md, ch07) | $10–50M tape-out for a one-developer AGPL project; "speculative" is generous; "north star" is honest | Low (admitted) |
| "Reproducible builds (R8)" (§4.1) | Cargo.lock + toolchain pin; no diffoscope attestation, no `cargo vet` chain | Medium |
| "Phase 0 audit document" (dossier §3.9 implicitly) | `docs/audits/phase-0.md` "Sign-off: pending"; self-audit, not external | Medium |
| "No comparable production OS treats drivers as signed WASM" (§1, §3.3) | True for WASM; Tock OS has language-isolated drivers in Rust source as a precedent | Minor |
| "Heli stacks two structurally different mechanisms" vs. "Firecracker has one" (§1) | Firecracker also has two (KVM hardware virt + Linux guest kernel isolation + seccomp); the comparison is rhetorically tilted | Minor |

---

## 4. What would have to be true for Heli to succeed

The necessary conditions, named explicitly:

1. **A second engineer joins and stays.** Solo founder + LLM collaboration is sufficient for a research artifact; it is not sufficient for a security-critical production OS. Either (a) Gustavo hires a co-founder caliber kernel engineer (probably 1–2 person-years to find and onboard), or (b) the project attracts a sustained contributor community (Redox's volunteer model — slow and rarely production-credible), or (c) an institutional backer materializes (academic lab, sovereign agency, foundation).

2. **A funding mechanism that AGPL doesn't poison.** Either (a) the founder is independently wealthy or grant-funded for 5+ years, (b) an institution (university, sovereign agency, foundation) takes the project on under explicit AGPL acceptance, or (c) the project relicenses to a more commercially-flexible terms (BSD/Apache for the kernel, AGPL for the userspace tooling) before serious investment is sought. Without one of these, the project hits a sustainability wall around year 3–5.

3. **wasmi gets either formally verified or replaced with something that has been.** The current Tier-0 trust footprint depends on wasmi being correct. wasmi is a moving target maintained by the Parity team for embedded chains; it is not designed as a high-assurance runtime. Either (a) a Phase-4 formal-verification collaboration with an academic group materializes and succeeds (5+ years), (b) Heli migrates to a verified-or-verifiable alternative (Walrus? Wasmer's lite runtime? A bespoke verified wasm interpreter?), or (c) Heli accepts wasmi as a TCB component and prioritizes wasmi-fuzzing as a continuous CI burden.

4. **A first paying customer who is not a hyperscaler and not a sovereign ministry.** Both of the dossier's primary audience archetypes (LATAM ministry, Chinese sovereign-fund) are extremely-long-cycle buyers. Heli needs an *intermediate* customer — a regional CSP, a privacy-focused hosting provider, a research consortium, a single-tenant security-conscious enterprise — to ground the product in real workloads and surface the operational gaps. Without that, Phases 2/3 build on a hypothetical use case.

5. **A real Phase-1 audit by an external party.** The dossier and CLAUDE.md call for external audit at Phase 3. That is too late. An interim Phase-1 external review by a credible security firm (Trail of Bits, NCC Group, Cure53) at a $50K–200K commitment, followed up at every major-version, would convert the security story from "we believe it is secure" to "an outside party signed off on at least this version." Without that, the security posture remains aspirational.

6. **The convergence thesis remains marketing, not load-bearing architecture.** If the cultural framing scales (the blog post lands, the framing resonates), it is a real asset. If it doesn't, the project must still stand on the technical work alone. The current dossier risks tying the *technical* credibility to the *cultural* framing in a way that, if one fails, the other goes with it. The technical story should be defensible *without* the convergence pitch, and the convergence pitch should be a layer of values on top, not a substitute for technical maturity.

7. **The smoltcp / network architecture commits to a multi-tenant socket model before Phase 2.** The current single-smoltcp-instance design needs a chosen, written, implemented answer to "how do 10k tenants share one TCP stack" before any HTTP-on-Heli demo can be honest.

8. **The Phase-4 SoC bet is explicitly retired or explicitly funded.** Custom silicon is a $10M+ commitment that cannot be done by one developer. Either the project sponsors-up to fund a tapeout (requires institutional backer in Phase 2) or removes Phase 4 from the roadmap and frames the MMU-free direction as "structurally compatible if a hardware partner materializes." Currently it sits in roadmap limbo.

If 1–5 happen in the next 12–24 months, Heli is a real project on a long trajectory. If they don't, Heli stays as it currently appears — a single-author research artifact with a beautiful manifesto.

---

## 5. Falsifiable milestones to revisit in 12–18 months

A reviewer revisiting in late 2027 should check, in order of importance:

- [ ] Has a second engineer landed a non-trivial PR (>200 lines of kernel code) and stayed for >6 months?
- [ ] Has Phase 1c (network on silicon) actually shipped with ARP/ICMP/TCP working at >99% reliability under load?
- [ ] Has any external security review been commissioned and published, even at the "report from a credible audit firm" level?
- [ ] Has wasmi been pinned to a specific version with documented fuzz coverage (the fuzz harness using wasmi 1.0.9 while kernel uses 0.32.3 is fixed)?
- [ ] Has the production signing key story moved beyond "decided at the time" to a documented HSM-or-equivalent custody plan?
- [ ] Has the cap-system Kani proof set expanded to include revoke cascade and IPC cap transfer?
- [ ] Has a first non-trivial Tier-1 workload (HTTP service, even a static-file server) shipped end-to-end?
- [ ] Has any external party (university, agency, foundation, company) publicly endorsed/funded/committed-to the project?
- [ ] Has Phase 2 (HTTP, oci2wasm, hardware crypto) shipped any artifact, or has the roadmap slipped without a replacement?
- [ ] Has the LATAM-sovereignty pitch been tested against at least one real procurement officer in writing, and what did they say?

Hit rate above 5/10 makes Heli a project to revisit seriously. Below 3/10 means the trajectory hasn't materially changed and the previous skepticism stands.

---

## 6. The kindest plausible reading

The strongest single thing about Heli is the *integration* of three things that are individually credible: (a) seL4-shape capability discipline, (b) WASM-as-process-boundary discipline, (c) RISC-V open-ISA discipline. Each of those alone is a real research lineage with serious prior art. The combination — done at a level a solo developer can hold in their head, with explicit invariants, with adversarial tests on the trust boundaries, with a real signed-driver pipeline, with a working silicon photograph — is rare. Most projects that attempt this combination either (i) start in academia and never ship (Singularity, RedLeaf, Theseus), (ii) ship in narrow embedded markets (Tock, Hubris), or (iii) commit to one of the three at the expense of the other two (Firecracker chooses #c+narrow-Rust; Tock chooses #a+Rust-not-WASM; Cloudflare chooses #b at hyperscale on commodity hardware). Heli's bet is that all three can be done at once at a level small enough to formally verify eventually.

If the bet pays off — if wasmi gets verified, if a second engineer joins, if external audit happens, if a first non-ministry customer materializes, if the convergence thesis lands as marketing on top of demonstrated technical seriousness — Heli is a credible contributor to a real long-term shift in how sovereign infrastructure gets built. The architectural choices on disk today are largely the right ones for that future; the discipline (CLAUDE.md, invariants, PR template, Kani harnesses) is unusual for a solo project and signals that the founder is serious about *eventually* meeting a high bar.

The cultural framing, taken charitably, is the founder's authentic attempt to ground a technical project in the value system of the communities he intends it to serve. That is rarer than it should be in this market; most sovereign-tech pitches are written for VCs in San Francisco and back-translated to the customer. Heli reads as the inverse: written for the customer in Oaxaca and back-translated to the funder. Whether or not the convergence thesis survives anthropological scrutiny in detail, its existence as a *value statement* is closer to the procurement reality in LATAM than yet-another-Apache-2.0-Linux-fork pitched out of Palo Alto.

The right frame for a sympathetic reader is: *Heli is a five-year option on a sovereign-tech shift, currently held by one disciplined developer. The option is cheap (one developer is cheap), the strike price (institutional adoption) is high, and the time-to-expiry (the next 24 months of phase-progress) is the diagnostic.* If you believe the world is bending toward open-ISA sovereign infrastructure on a 10-year horizon — a defensible bet — Heli is one of the *cleanest* solo expressions of that bet currently visible. It is not the *most-funded* expression (that would be Oxide-but-for-cloud, which doesn't exist), nor the *most-academic* (seL4/ATMO), nor the *most-pragmatic* (Linux-on-RISC-V via Debian-port), but it is the most *coherent* between technical architecture and value statement. That coherence is what would make it worth a small bet from a patient backer, and worth ignoring from anyone needing returns under 7 years.

End of memo.
