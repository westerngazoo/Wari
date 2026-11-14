# Documentación — wari

WASM-native OS for RISC-V — capability-based, two-tier sandbox

## Índice

| Documento | Descripción |
|-----------|-------------|
| [architecture.md](./architecture.md) | Arquitectura detallada (documento canónico) |
| [architecture-overview.md](./architecture-overview.md) | Resumen con mapa de módulos y diagrama |
| [code-walkthrough.md](./code-walkthrough.md) | Recorrido por el código fuente |
| [flows.md](./flows.md) | Flujos Mermaid + PlantUML |
| [design-philosophy.md](./design-philosophy.md) | Principios de diseño y decisiones |
| [diagrams/](./diagrams/) | Fuentes `.puml` exportables |

## Stack

Rust 1.95, riscv64 kernel, wasm32 drivers/apps, wasmi, SBI, Kani proofs

## Documentación existente

- `docs/architecture.md`
- `docs/invariants.md`
- `docs/book/`
- `docs/STATE-OF-PLAY.md`

## Proyectos relacionados

- [clifford](../clifford/)
- [ufl](../ufl/)

---

*Generado como parte de la documentación unificada de `~/projects`.*
