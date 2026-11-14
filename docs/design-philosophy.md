# Filosofía de diseño — wari

## Principios

- WASM-only process model — no ELF in client ABI.
- Two-tier sandbox: Tier-1 MMU+WASM, Tier-2 WASM+MMIO.
- Minimal verifiable kernel (~5–10 KLOC target).
- Security tests mandatory per driver PR.

## Contexto

Minimal Tier-0 kernel; Tier-1 apps and Tier-2 drivers as signed WASM; boots QEMU and VisionFive 2.

## Trade-offs explícitos

Este proyecto prioriza coherencia con los principios anteriores sobre conveniencia ad-hoc.
Cuando una decisión contradice un principio, debe documentarse como ADR o RFC.

## Relación con el ecosistema

- **clifford**
- **ufl**
