# Flujos — wari

## Flujo principal (Mermaid)

```mermaid
flowchart TB
subgraph T0[Tier 0 Kernel]
CAP[capability cspace]
WASM[WASM runtime]
end
subgraph T1[Tier 1 Apps]
APP[signed WASM app]
end
subgraph T2[Tier 2 Drivers]
DRV[uart / net driver]
end
BOOT[boot.S] --> CAP
CAP --> WASM
WASM --> APP
WASM --> DRV
APP -->|syscall| CAP
DRV -->|MMIO IPC| CAP
```

## Descripción paso a paso

1. **Boot** — `boot.S` → `kmain` → paging/traps → capability boot grants.
2. **Runtime** — WASM engine loads signed Tier-1/Tier-2 modules.
3. **Syscall** — cap check → dispatch → driver IPC for MMIO/IRQ.
4. **Driver** — Tier-2 signed WASM (uart/net) via host functions + grants.

## Secuencia (PlantUML)

Fuente: [`diagrams/flow-sequence.puml`](./diagrams/flow-sequence.puml)

```plantuml
@startuml
title wari — secuencia principal

participant "Boot" as Boot0
participant "Runtime" as Runtime1
participant "Syscall" as Syscall2
participant "Driver" as Driver3

Boot0 -> Runtime1: WASM engine loads signed Tier-1/Tier-2 modules.
Runtime1 -> Syscall2: cap check → dispatch → driver IPC for MMIO/IRQ.
Syscall2 -> Driver3: Tier-2 signed WASM (uart/net) via host functions + grants.

@enduml
```

## Componentes / estados (PlantUML)

Fuente: [`diagrams/flow-architecture.puml`](./diagrams/flow-architecture.puml)

```plantuml
@startuml
title wari — flujo de componentes
start
:BOOTboot.S;
:CAP;
:WASM;
:APP;
:DRV;
stop

@enduml
```

## Estados y casos borde

Consulta los tests de integración y los RFC/requirements del proyecto para flujos de error y recuperación.
