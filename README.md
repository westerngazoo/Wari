<div align="center">

# 和力 · Wari

### Hé Lì — Harmonious Force

*Where ancient reciprocity networks meet modern sovereign infrastructure.*

![status](https://img.shields.io/badge/phase-1c%20silicon%20bringup-blue)
![license](https://img.shields.io/badge/license-AGPL--3.0-green)
![arch](https://img.shields.io/badge/arch-RISC--V%20RV64GC-orange)
![runtime](https://img.shields.io/badge/runtime-WASM%20native-purple)

</div>

---

## The convergence thesis

Three civilizational traditions, validated independently across three
millennia, encode the same operating principle:

| Tradition | Term | Principle |
|---|---|---|
| **Andean (Wari, Inca)** | *Ayni* | Sacred reciprocity. Every gift creates an obligation. |
| **Mexican (Mesoamerican)** | *Tequio* | Communal labor. The collective is built by individual contributions. |
| **Chinese (Confucian)** | *和 Hé* | Structural harmony. Order emerges from properly aligned relationships. |

**Unified principle:** every node has I/O obligations to the network.
The network's health is cosmic law.

This is not a marketing metaphor. **It is the architecture.** Wari's
capability system, two-tier sandbox, and explicit IPC contracts are
the same logic Andean *ayni* encoded in quipu strings, that Mexican
*tequio* still organizes village labor by, that Chinese statecraft
codified into bureaucratic ritual — implemented in 5–10 KLOC of
formally-verifiable Rust on open silicon.

---

<div align="center">

![Wari booting on a StarFive VisionFive 2 — JH7110 RISC-V silicon, April 2026](docs/assets/first-boot-vf2.png)

*Wari v0, booting on real RISC-V silicon (StarFive VisionFive 2, JH7110).
First "Hello from Wari" on hardware — April 2026.*

</div>

---

## What Wari is

A **WASM-native operating system for RISC-V**, released under AGPL-3.0,
with no telemetry and no undocumented interfaces. Designed for formal
verification. Intended for sovereign cloud infrastructure in Latin
America, with structural alignment to Chinese long-cycle investment
horizons.

- **Not a commercial product.** A shared engineering asset.
- **Not a fork.** Wari is WASM-native from boot zero — no Linux
  inheritance, no ELF in the customer ABI, ever.
- **Not a trend.** A 3,000-year civilizational lineage rendered into
  silicon-and-Rust at the level where it can outlive its builders.

## Why it's structurally defensible

| | |
|---|---|
| **🕸️ Network-first identity** | The brand is built on reciprocity logic, not a product feature. Cannot be copied without copying the entire philosophy. |
| **🌐 Tri-cultural fluency** | Speaks natively to Andean, Mexican, and Chinese sensibilities without being foreign to any of them. |
| **⏳ Long time horizon** | Chinese capital thinks dynastically. The brand is built on 1,400-year-old validated social technology. |
| **🛡️ Authentic origin** | Wari is not an aesthetic. It is a real civilizational lineage with archaeological depth — and a working OS booting on real silicon. |

---

## Market context

| Surface | Scale |
|---|---|
| LATAM consumer market by 2030 | **$4.2T** |
| Chinese middle-class consumers | **1.4B** |
| Mexican domestic market | **130M** |
| Years of validated cultural logic | **3,000+** |

Cloud infrastructure today resides in specific jurisdictions — Virginia,
Dublin, Singapore — operated by a handful of providers governed by laws
the user countries did not shape. When a hospital in Oaxaca stores
patient records, those records sit on hardware it cannot inspect, in
territory it does not control. This is not a peripheral technical
concern. It is a question of digital sovereignty.

Wari is one contribution to that goal: a stack whose **ownership,
governance, and inspectability** can be verified directly by the
institutions that depend on it.

---

## Design principles

- **WASM-only process model.** No ELF in the client ABI, at any layer.
- **Two-tier sandbox.** Client code (Tier 1, MMU + WASM) and drivers
  (Tier 2, WASM-only) are both WASM modules, executed at distinct
  privilege levels through capability grants.
- **Minimal native kernel.** Tier 0 is approximately 5–10 KLOC of
  Rust, sized for formal verification.
- **Sovereign technology stack.** Open hardware (RISC-V), open
  drivers (auditable `.wasm`), confidential computing (CoVE, Phase 3),
  and custom silicon (GAPU FPGA, Phase 3).

See [`docs/architecture.md`](docs/architecture.md) for the full
technical rationale.

---

## A statement from The Geese Collective

> Cloud infrastructure is not abstract. It resides in specific
> jurisdictions — Virginia, Dublin, Singapore — operated by a small
> number of providers and governed by the laws of the countries that
> host them. The concentration of global compute and data services in
> the hands of three providers has become a structural risk for the
> public sector across the global south. Continuity of service, data
> residency, lawful access, and supply-chain transparency all depend
> on decisions made elsewhere. Latin American institutions deserve
> infrastructure whose ownership, governance, and inspectability they
> can verify directly.
>
> Wari is one contribution to that goal: a WASM-native operating system
> for RISC-V, released under AGPL-3.0, with no telemetry and no
> undocumented interfaces. It is not a commercial product. It is a
> shared engineering asset, intended to be adopted, audited, and
> extended by the institutions that depend on it.
>
> **Built to be shared, not rented.**

---

## Status

**Phase 1c — silicon bring-up.** Booting on VisionFive 2 (JH7110,
RV64GC). GMAC0 + smoltcp wired; ARP/ICMP path under final calibration.
TCP socket layer queued for Net-6c. The complete phase ladder lives in
[`docs/STATE-OF-PLAY.md`](docs/STATE-OF-PLAY.md).

| Phase | What | Status |
|---|---|---|
| 0 | Boot + UART + WASM noop | ✅ shipped |
| 1a | Tier-1 hello.wasm on silicon | ✅ shipped |
| 1b | Capabilities + scheduler + IPC + Tier-2 drivers | ✅ shipped |
| 1c | Network driver (VirtIO + JH7110 GMAC0) | 🔧 in progress |
| 2 | Smol Tier-1 stack (HTTP, JSON) | queued |
| 3 | Confidential computing + GAPU FPGA coprocessor | designed |

## Running on VisionFive 2

```bash
# Dev machine
make kernel-vf2              # build kernel + driver wasm, sign, verify
make verify                  # confirm all artifacts at the same build tag

# Push, then on the VF2:
wari upgrade                 # pull origin, flash /boot/kernel.bin, verify md5
wari go -y                   # countdown + reboot into Wari
```

Initial bring-up (cloning the repository on the device and installing
the `wari` shell function) is documented in
[`docs/vf2-bringup.md`](docs/vf2-bringup.md).

## Documentation

| Document | Purpose |
|---|---|
| [`CLAUDE.md`](CLAUDE.md) | Project rules, invariants, phase ladder |
| [`docs/manifesto.md`](docs/manifesto.md) | 和力 — the convergence thesis in full |
| [`docs/architecture.md`](docs/architecture.md) | Technical architecture |
| [`docs/prior-art.md`](docs/prior-art.md) | What we adopt, what we reject |
| [`docs/invariants.md`](docs/invariants.md) | The `INV-N` catalogue |
| [`docs/security-model.md`](docs/security-model.md) | Threat model |
| [`docs/testing.md`](docs/testing.md) | Test layers + adversarial coverage |
| [`docs/pr-workflow.md`](docs/pr-workflow.md) | How to propose a change |
| [`docs/book/`](docs/book/) | *The Goose Factor*, Volume 2 — design rationale |

For Claude / LLM-assisted contributors, the session-pickup brief lives
at [`docs/CLAUDE-CONTEXT.md`](docs/CLAUDE-CONTEXT.md).

## License

[AGPL-3.0-only](LICENSE). Built to be shared, not rented.

---

<div align="center">

**和力 · Hé Lì**

*Soberanía tecnológica, tierra y libertad.*

— The Geese Collective

</div>
