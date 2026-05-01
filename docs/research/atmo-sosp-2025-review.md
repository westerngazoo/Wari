# ATMO (Atmosphere) — SOSP 2025 — Review for Wari

**Reviewer:** Claude (co-architect)
**Date:** 2026-04-26
**Status:** First-pass prior-art review.
**Source caveat:** The PDF at
`C:\Users\GustavoDelgadillo\Downloads\2025-sosp-atmo.pdf` could not be
rendered to text in the current sandbox (the local `pdftotext`/`pdftoppm`
tools were blocked from executing on the file, and `WebFetch` was denied).
This review is therefore based on (a) the SOSP 2025 ACM listing, (b) the
Mars Research Group project page, (c) the 2023 KISV precursor paper
("Atmosphere: Towards Practical Verified Kernels in Rust"), and (d) the
SOSP 2025 best-paper announcement. Page numbers are deliberately omitted
because I cannot verify them; before this review is filed in
`docs/prior-art.md`, Gustavo (or a follow-up pass with PDF extraction
working) should spot-check the technical claims against the actual PDF.

---

## 1. Executive summary

- **ATMO = "Atmosphere: Practical Verified Kernels with Rust and Verus."**
  A full-functionality L4-style microkernel written in Rust and proven
  functionally correct (refinement of a high-level spec) using the Verus
  verification toolchain.
- **Authors:** Xiangdong Chen, Zhaofeng Li, Jerry Zhang, Vikram Narayanan,
  Anton Burtsev (University of Utah / Mars Research Group). SOSP 2025,
  Seoul, October 2025. **Best Paper Award.**
- **Headline result:** functional-correctness verification of a feature-rich
  microkernel at a **proof-to-code ratio of ~7.5:1**, versus 19:1 for
  seL4 (Isabelle/HOL) and 20:1 for CertiKOS (Coq). Total project effort:
  ~2 person-years over ~1 calendar year.
- **Why it matters:** ATMO is the first practical demonstration that
  Rust + Verus can verify the kind of kernel surface area Wari wants
  (page tables, memory allocators, capability-style endpoints, threads),
  at an effort budget that a small team can plausibly carry.
- **Direct relevance to Wari:** **High.** Same source language (Rust),
  same kernel ancestry (L4/seL4-style capabilities + endpoints), same
  small-TCB philosophy. The verification methodology is the single most
  important external input to Wari's Phase 1b cap-system design and the
  long-term formal-verification roadmap.

---

## 2. Technical contribution

The novel claims, as best I can reconstruct without the PDF:

1. **Verus-as-spec-language for an OS kernel.** Verus is a Dafny-like
   SMT-backed verifier embedded in the Rust toolchain; it lets proofs
   live alongside the implementation in the same source files, in ghost
   code that compiles away. Prior verified kernels (seL4 / Isabelle,
   CertiKOS / Coq) used external proof assistants and a separately
   maintained refinement chain. ATMO collapses that gap.
2. **Proof-to-code ratio of ~7.5:1** — an order-of-magnitude reduction
   in human proof effort compared to seL4 (19:1) and CertiKOS (20:1).
   This is the paper's main empirical claim and the basis for the
   "practical" qualifier in the title.
3. **A working L4-class kernel surface verified end-to-end.**
   The kernel exposes: address spaces, page tables, coarse-grained
   memory management, threads, and **endpoints that double as
   capabilities** (endpoints can be passed between processes to
   establish shared-memory regions). Drivers, the network stack, and
   filesystems live in user space.
4. **Verification of low-level invariants natively in Rust.** Linked
   lists, slab/page allocators, and page-table walks — the data
   structures that have historically resisted automated reasoning —
   are reasoned about with Verus's `tracked` ghost state and linear
   permission tokens, without dropping out of Rust into a separate IR.
5. **Engineering-economics result.** ~2 person-years for a
   feature-rich verified microkernel reframes formal verification from
   a decade-scale national-lab project to a single-grad-student-thesis
   project. This is the contribution most likely to outlive the
   specific artifact.

The 2023 KISV precursor paper is essentially the "we think this will
work" version; the SOSP 2025 paper is the "we shipped it and here are
the proofs" version.

---

## 3. Relevance to Wari — direct

ATMO and Wari are close cousins, with one large divergence (WASM
userspace) and one small divergence (RISC-V vs x86_64). Touchpoints:

### 3.1 Capability-style endpoints
ATMO's endpoints serve double duty as IPC channels and as caps for
sharing memory regions — essentially the seL4 endpoint+frame model
collapsed into one object. **What Wari can take:** validation that the
endpoint-as-capability pattern is verifiable in Rust under modern
tooling. Wari's Phase 1b design already uses 4 kernel object kinds
(Endpoint / Notification / Untyped / Frame); ATMO suggests this is
on the high-complexity end of what Verus can handle today, which is
useful sizing information. **What to watch for:** how ATMO handles
**cap derivation, revocation, and the CDT (capability derivation
tree)**. Wari INV-10 (rights monotonicity), INV-17 (generation
counters / ABA), and parent-cascade revocation are exactly the proof
obligations that historically dominated the seL4 effort. If ATMO
sidesteps revocation (some L4 variants do), that's a meaningful
asymmetry — Wari cannot sidestep it without weakening the security
model.

### 3.2 Verification methodology (Verus)
This is the highest-leverage takeaway. Wari's stated plan is
"Kani harnesses now, Coq-style proofs later via academic
collaboration." ATMO's result strongly suggests the **middle of that
sandwich is wrong**: Verus is now a credible target that sits between
Kani (bounded model checking, finds bugs but does not prove absence)
and full Coq refinement (multi-year effort). Wari should evaluate
Verus seriously as the Phase 4 verification target, before committing
to Coq. The proof-to-code ratio matters enormously for an 8 KLOC
kernel: 7.5:1 = ~60 KLOC of proof, plausibly tractable; 19:1 = 152
KLOC, not tractable for a one-or-two-person project.

### 3.3 Microkernel design / TCB sizing
ATMO confirms that the L4 surface (address spaces, page tables,
threads, endpoints, untyped memory) is verifiable at small scale
without sacrificing functionality. Wari's TCB target ("auditable in
<1 week, ~8 KLOC") sits in the same zone. **Watch for:** the exact
LOC breakdown ATMO reports — kernel core vs proof vs trusted-spec.
The "spec" file is part of the TCB even when proven. Wari's
audit-week claim has to include the spec.

### 3.4 RISC-V gap
ATMO is, to the best of my knowledge from the public materials,
**x86_64-only**. seL4 took years to retarget from ARM to RISC-V; the
verification doesn't port for free because architectural assumptions
(memory model, page-table format, TLB semantics, atomic primitives)
leak into proofs. **Implication for Wari:** if Wari adopts Verus,
expect to write its own Sv39 page-table proofs and its own RV64GC
memory-model assumptions. There is no shortcut. This is *also* an
opportunity: Wari + Verus + RISC-V is a publishable artifact in its
own right.

### 3.5 No WASM in ATMO
ATMO runs unmodified user binaries (ELF, presumably). Wari's
WASM-in-userspace model is **strictly outside** ATMO's verification
scope. ATMO does not reduce the Wari-specific verification burden of
proving WASM sandbox soundness, the Tier-1/Tier-2 split, or the
double-sandbox property (MMU + WASM). Those remain Wari's own
problem, and they are arguably the most novel verification targets in
the Wari roadmap.

### 3.6 Sovereign / auditable computing
ATMO's "best paper at SOSP" status validates the **market** for
verified-kernel-as-trust-anchor narratives. Wari's pitch to LATAM
gov / banks / SOEs is strengthened by being able to point at a
contemporaneous, peer-reviewed, best-paper-honored example of the
genre. Cite ATMO in Wari's positioning materials.

---

## 4. Relevance to Wari — indirect

- **Spec-first kernel development as a discipline.** Verus pushes
  authors to write a high-level spec, then refine. Wari's invariants
  doc (`docs/invariants.md`, INV-1 through INV-N) is informally
  playing this role. Even before adopting Verus, Wari can borrow
  ATMO's pattern: the invariants doc should be reorganized so that
  every invariant maps to a future ghost-state predicate. Phase
  2/3/4 verification will be much cheaper if the spec is already
  factored that way.
- **User-space drivers as a verification reduction.** ATMO keeps
  drivers out of the kernel, so they're outside the verified TCB.
  Wari's Tier-2 signed drivers are conceptually similar (S-mode but
  WASM-sandboxed and outside the Rust kernel TCB). ATMO is empirical
  evidence that this split keeps the verification problem bounded.
- **Endpoint-mediated shared memory as the canonical IPC.** ATMO
  collapses "send a message" and "share a page" into one
  endpoint-driven mechanism. Wari should look at whether its
  Endpoint + Frame separation buys anything that ATMO's unified model
  doesn't, or whether the separation just doubles the proof surface.
- **Experimental methodology.** Whatever benchmarks ATMO reports
  (likely lmbench-style IPC latency, page-fault round-trip, syscall
  cost), Wari should reproduce the same metrics on QEMU-virt RV64
  and on VF2 silicon. This gives Wari an apples-to-near-apples
  number to publish in any future paper, and forces honest
  bookkeeping about the interpreter-vs-JIT performance gap.
- **Best-paper visibility in 2025.** ATMO will be the reference
  others compare against for the next 3–5 years. Wari should expect
  reviewers and grant committees to ask "why not just use ATMO?"
  The answer is: WASM + RISC-V + sovereign-stack + AGPL. That answer
  needs to be one paragraph in every Wari pitch document.

---

## 5. Risks and rejections

Things in ATMO that Wari should **not** adopt:

1. **Trusting Verus's TCB without scrutiny.** Verus is younger than
   Isabelle/HOL or Coq; its soundness rests on the Rust frontend, the
   SMT solver (Z3), and the encoding from Verus's logic to SMT.
   Adopting Verus means inheriting that TCB. Wari's "auditable in
   <1 week" pitch becomes harder to defend if a reviewer counts
   Z3 + the Verus encoder as part of the TCB. This is a real
   argument — not a deal-breaker, but it must be stated honestly.
2. **x86-shaped abstractions.** If ATMO's endpoint or page-table API
   embeds x86_64 assumptions (e.g., 4-level paging, specific PCID/ASID
   semantics), Wari should not lift them wholesale onto Sv39. Adapt,
   don't copy.
3. **Monolithic endpoint object.** ATMO's "endpoint = IPC channel +
   memory-share cap" is elegant but it merges two security domains.
   Wari's stricter Endpoint/Notification/Frame separation is closer
   to seL4 and arguably stronger for high-assurance audits. Don't
   collapse it just because ATMO did.
4. **No JIT — already aligned, but worth restating.** Nothing in ATMO
   suggests adding a JIT to a verified kernel; it would explode the
   verification surface. This **reinforces** Wari's no-JIT thesis.
   If anything, ATMO is evidence that Wari's interpreter-first +
   `Zwari` extension trajectory is the right path for a verifiable
   stack.
5. **Two person-years is the floor, not the ceiling.** ATMO reports
   ~2 PY for a 1-architecture, no-WASM kernel by an experienced OS-
   verification group. Wari's verification budget is larger because
   of (a) RISC-V proofs, (b) WASM-sandbox proofs, (c) the
   Tier-1/Tier-2 invariants. Realistic Wari-Verus budget should
   plan for 4–6 PY, not 2.

---

## 6. Open questions for Gustavo

1. **Verus vs Kani vs Coq — should Wari's Phase 4 verification
   target be Verus?** ATMO is strong evidence that the answer is
   yes. Decision-quality question; worth a half-day of reading the
   Verus tutorial and the ATMO artifact before committing.
2. **Does ATMO's endpoint-as-capability design challenge Wari's
   four-object cap design (Endpoint/Notification/Untyped/Frame)?**
   Specifically: is there a cleaner three-object or two-object
   design that still gives Wari INV-10 and revocation? Worth a
   design memo.
3. **Can Wari reproduce ATMO's lmbench-style numbers on
   QEMU-virt-RV64?** If so, that's a Phase 1b deliverable (and a
   slide for the book). If WASM tax makes it embarrassing, that's
   useful information for the `Zwari` Phase 3 pitch.
4. **What is in ATMO's trusted spec?** Verified-correct-against-spec
   is not the same as bug-free; the spec itself can be wrong. Read
   the ATMO spec file when the artifact drops; use it as a template
   for Wari's own future spec.
5. **Should Wari reach out to the Mars Research Group for
   collaboration?** They have demonstrated Verus-on-kernel
   expertise. Wari has RISC-V + WASM. The intersection is a joint
   paper.

---

## 7. Citation block

```
Xiangdong Chen, Zhaofeng Li, Jerry Zhang, Vikram Narayanan,
and Anton Burtsev.
"Atmosphere: Practical Verified Kernels with Rust and Verus."
In Proceedings of the ACM SIGOPS 31st Symposium on Operating
Systems Principles (SOSP '25), Seoul, Republic of Korea,
October 2025. ACM. Best Paper Award.
DOI: 10.1145/3731569.3764821
URL: https://dl.acm.org/doi/10.1145/3731569.3764821
Project page: https://mars-research.github.io/projects/atmo/
```

Precursor (open access, useful background reading):

```
Xiangdong Chen et al.
"Atmosphere: Towards Practical Verified Kernels in Rust."
In Proceedings of the 1st Workshop on Kernel Isolation, Safety
and Verification (KISV '23). ACM, 2023.
DOI: 10.1145/3625275.3625401
PDF: https://mars-research.github.io/doc/2023-kisv-atmo.pdf
NSF copy: https://par.nsf.gov/servlets/purl/10549642
```

---

## Reviewer note on confidence

- **High confidence:** authors, venue, best-paper status, language
  (Rust), verifier (Verus), high-level architecture (L4-style),
  proof-to-code ratio of 7.5:1, ~2 PY effort.
- **Medium confidence:** endpoint-as-capability detail, user-space
  driver model, no WASM, x86_64-only target. Sourced from project
  page and KISV 2023 precursor; the SOSP 2025 paper may have
  expanded scope.
- **Low confidence / not verified:** specific page numbers,
  benchmark numbers, exact LOC, exact list of verified properties.
  These should be filled in on a second pass once the PDF can be
  read directly.

Recommendation: keep this review as a **scoping document**, and
schedule a follow-up pass once `pdftotext` is available in the
sandbox or once the artifact (with the published PDF + proofs)
is downloaded into the repo. At that point the page citations and
benchmark numbers can be filled in inline without altering the
shape of the analysis above.
