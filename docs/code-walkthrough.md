# Recorrido del código — wari

Guía orientada a desarrolladores para entender dónde vive cada responsabilidad.

### 1. Boot

`boot.S` → `kmain` → paging/traps → capability boot grants.

### 2. Runtime

WASM engine loads signed Tier-1/Tier-2 modules.

### 3. Syscall

cap check → dispatch → driver IPC for MMIO/IRQ.

### 4. Driver

Tier-2 signed WASM (uart/net) via host functions + grants.

## Punto de entrada recomendado

Empieza por el README del proyecto y el módulo/crate principal listado en la documentación de arquitectura.
