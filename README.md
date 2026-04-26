# EZLTN — Ejército Zapatista de Liberación Tecnológica Nacional

**Un comunicado.**

La nube no está en el cielo. Está en Virginia, en Dublín, en
Singapur. Tiene dueños, tiene cercas, tiene aduanas. Cuando un
hospital de Oaxaca guarda los expedientes de sus pacientes, los
guarda en territorio que no es suyo, bajo leyes que no votó, en
hardware que no puede abrir. Esto no es un detalle técnico. Es
la forma del siglo XXI de una pelea vieja.

En 1910 dijimos *tierra y libertad* porque la tierra era el medio
de producción y nos la habían quitado. En 2026 decimos *soberanía
tecnológica, tierra y libertad* porque el medio de producción
ahora también es el datacenter, el kernel, el silicio, el modelo
de IA que decide quién recibe crédito y quién no. Tres empresas
hospedan la vida digital del sur global. Eso es colonialismo con
mejor marketing.

No estamos contra la tecnología. Estamos por quién la posee. Wari
es una herramienta — un sistema operativo nativo de WASM para
RISC-V, AGPL-3.0, sin telemetría, sin puertas traseras, sin
permiso pedido a nadie. No es un producto. No se renta. No se
vende. Se comparte, porque las herramientas se comparten — no se
alquilan.

No pedimos permiso. Construimos.

— EZLTN

---

![Wari corriendo en silicio real — VisionFive 2, primer boot](docs/assets/first-boot-vf2.png)

*Wari v0 build 12, booteando en una VisionFive 2 (JH7110, RISC-V RV64GC).
Salida UART por COM7. Primer "Hello from Wari" sobre silicio real —
abril 2026.*

---

# Wari

Un sistema operativo nativo de WASM para RISC-V, formalmente
verificable, dirigido a infraestructura cloud soberana en
Latinoamérica.

**Estado:** Phase 1a cerrada. Booteando en silicio VisionFive 2.
Phase 1b (capacidades + scheduler + IPC + driver Tier-2 de red)
en planeación.

## Qué hace a Wari diferente

- **Modelo de proceso WASM-only.** Nada de ELF en el ABI de cliente,
  jamás.
- **Sandbox de dos tiers.** Código de cliente (Tier 1, MMU + WASM) y
  drivers (Tier 2, WASM-only) son ambos módulos WASM, ejecutados con
  privilegios distintos vía grants de capacidades.
- **Kernel nativo diminuto.** Tier 0 son ~5–10 KLOC de Rust, escala
  de verificación formal.
- **Soberanía LATAM.** Hardware abierto (RISC-V) + drivers abiertos
  (`.wasm` auditable) + computación confidencial (CoVE, Phase 3) +
  silicio custom (GAPU FPGA, Phase 3).

Ver `docs/book/` para la derivación narrativa (Volume 2 de
*The Goose Factor*).

## Corriendo en VisionFive 2

Phase-1a cerró con un harness de deploy funcional para silicio
RISC-V real. El flujo de día a día:

```bash
make deploy                  # máquina de dev: build wari.bin, push a GitHub
wari go                      # en la VF2: pull, copiar a /boot/kernel.bin, reboot
```

El bringup inicial (clonar el repo en el dispositivo, instalar la
función de shell `wari`) está documentado en
[`docs/vf2-bringup.md`](docs/vf2-bringup.md). La salida de boot
aparece en COM7 — ver el doc para el banner esperado.

## Empezando

Si llegaste a contribuir, leer en este orden:

1. `CLAUDE.md` — reglas, invariantes, fases
2. `docs/architecture.md` — arquitectura actual
3. `docs/prior-art.md` — qué heredamos y qué rechazamos
4. `docs/invariants.md` — el catálogo `INV-N`
5. `docs/pr-workflow.md` — cómo proponer un cambio
6. `docs/testing.md` — capas de test + cobertura adversarial
7. `docs/security-model.md` — modelo de amenazas
8. `docs/book/` — Wari, Volume 2

## Licencia

AGPL-3.0-only. Ver [`LICENSE`](LICENSE).

Las herramientas se comparten. No se alquilan.
