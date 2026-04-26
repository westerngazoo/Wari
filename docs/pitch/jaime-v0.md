# Wari — Café con Jaime · v0

> **Audiencia:** Jaime Aldana, Country Manager EPAM México. Relación
> personal. Café informal, 30-45 min.
>
> **Goal de la junta:** validar interés. NO es pitch comercial. Es
> "hermano, mira lo que estoy armando, ¿tiene sentido para el Garage?"
>
> **Forma:** 10 slides, ~3-5 minutos de presentación + 25-40 minutos
> de conversación abierta. La conversación es el producto, las slides
> son la excusa para que la conversación tenga estructura.
>
> **Tono:** brother-to-brother, no corporate. Slides austeras, sin logos,
> sin stock photos. La foto del primer boot hace todo el trabajo visual.

---

## Slide 1 — La foto

```
[ Pantalla completa: docs/assets/first-boot-vf2.png ]
```

**Caption (esquina inferior):**

> Wari v0 build 12 — VisionFive 2 (RISC-V RV64GC) — abril 2026

**Notas para Gustavo (30s):**

No digas nada por 5 segundos. Deja que mire la pantalla. Esa cadena
completa — U-Boot SPL → OpenSBI → Wari → tier-2 driver → "Hello from
Wari" → exit(0) — habla por sí sola para alguien que sabe leerla.

Después: *"Esto bootea en silicio RISC-V real desde hace tres semanas.
Es un kernel que escribí desde cero los fines de semana. Quería
mostrártelo antes de seguir."*

---

## Slide 2 — El manifiesto

> **Soberanía tecnológica, tierra y libertad.**
>
> No estamos contra la tecnología. Estamos por quién la posee. Wari es
> una herramienta — un sistema operativo nativo de WASM para RISC-V,
> AGPL-3.0, sin telemetría, sin puertas traseras, sin permiso pedido a
> nadie. No es un producto. No se renta. No se vende. Se comparte,
> porque las herramientas se comparten — no se alquilan.
>
> — EZLTN

**Notas para Gustavo (45s):**

*"Empiezo por aquí porque es el filtro de todo. Wari no es un proyecto
técnico. Es proyecto técnico CON postura. Si esa postura no le suena al
Garage, mejor lo sabemos hoy y termino mostrando otra cosa. Si le suena,
todo lo que sigue tiene contexto."*

Pausa. Léelo despacio. No te disculpes por el ángulo político. Es
deliberado y es el moat.

---

## Slide 3 — El problema

```
LATAM está rentando soberanía digital a 3 empresas extranjeras.

  ●  AWS    →  CLOUD Act subpoena alcance global
  ●  Azure  →  Microsoft + DOJ data sharing precedents
  ●  GCP    →  data localization "best effort"

Datos de ciudadanos LATAM, decisiones de IA sobre crédito,
historia clínica, expedientes fiscales, votación electrónica
— todo en jurisdicción que no votamos.

El comprador-soberano todavía no tiene tercera opción.
```

**Notas para Gustavo (45s):**

*"Esto no es noticia para ti — Garage MX ya pelea con esto en cada deal
gubernamental, cada banca, cada salud. Schrems II en Europa abrió la
conversación, BRICS la está acelerando, AMLO/Lula la politizaron.
La pregunta que tu cliente te hace hoy es: 'sí, sabemos que AWS es
incómodo, ¿qué uso en su lugar?' Y la respuesta hoy es 'OpenStack pero
caro, K8s en hardware tuyo pero igual sobre Linux que es 30 millones
de líneas que nadie audita.' Wari intenta ser la tercera opción."*

---

## Slide 4 — La hipótesis técnica

|  | Linux | seL4 | **Wari** |
|---|---|---|---|
| **TCB (LOC kernel)** | ~30,000,000 | ~10,000 | **~8,000** |
| **Auditable en** | Imposible solo | Semanas | **Semana, equipo de 3** |
| **Process model** | ELF + cgroups + namespaces | CapPOSIX | **WASM nativo, dos tiers** |
| **HW soberano** | Cualquier ISA | Cualquier ISA | **RISC-V nativo (no portado)** |
| **Verificación formal** | No | Sí (C, 11 años) | **Roadmap: Kani → Coq** |
| **Licencia** | GPL | BSD | **AGPL-3.0** |

**Notas para Gustavo (60s):**

*"La apuesta técnica son cuatro órdenes de magnitud menos código que
Linux. Eso no es marketing — eso cambia QUIÉN puede auditarlo. Linux
necesita la NSA o equivalente para auditar completo. Wari lo audita un
equipo de 3 ingenieros senior en una semana. Esa es la diferencia entre
'confío porque no tengo opción' y 'confío porque vi el código entero.'*

*Para clientes con regulación pesada — banca core, hacienda, salud,
defensa — esa diferencia vale dinero real. seL4 ya probó que el mercado
existe (lo usa Boeing, Lockheed, Qualcomm). Wari lleva esa idea a la era
WASM y a hardware abierto."*

---

## Slide 5 — Lo que YA está hecho

```
Phase 0 — QEMU demo                    ✓ cerrado
  · Bump allocator + page tables Sv39
  · WASM runtime (intérprete wasmi)
  · Driver Tier-2 firmado (UART NS16550)
  · App Tier-1 hello world WASI

Phase 1a — silicio real                ✓ cerrado
  · Cross-compile a RISC-V VF2
  · Drivers per-platform (NS16550 vs DW8250)
  · Deploy harness via GitHub
  · Boot completo en VisionFive 2

Phase 1b — multi-tenant                · arrancando
  · Capacidades + scheduler + IPC
  · Driver Tier-2 de red
  · Demo de carga: REST API en Tier-1
```

**Notas para Gustavo (45s):**

*"Esto no es PowerPoint. Es repo público en mi GitHub —
github.com/westerngazoo/Wari, AGPL, 8 PRs cerrados, foto del primer
boot que ya viste. Phase 0 cerrada en QEMU, Phase 1a cerrada en silicio
real. Todo trabajado fines de semana, fuera de horas EPAM, sin cruce
con propiedad intelectual de la empresa.*

*Phase 1b la arranco esta semana. Es el sprint que lleva a un demo
con carga real — donde podemos enseñarle a un cliente."*

---

## Slide 6 — Arquitectura

```
┌────────────────────────────────────────────────────┐
│  Tier 1 — Customer WASM (untrusted)                │
│  • MMU + WASM doble sandbox                        │
│  • Capacidades por instancia                       │
│  • U-mode RISC-V                                   │
└────────────────────────────────────────────────────┘
                       ↓ syscall (WASM imports)
┌────────────────────────────────────────────────────┐
│  Tier 2 — Driver WASM (firmado, semi-trusted)      │
│  • WASM-only sandbox                               │
│  • S-mode RISC-V, MMIO via host fns                │
│  • Cap MMIO restringida por validador              │
└────────────────────────────────────────────────────┘
                       ↓ host fns
┌────────────────────────────────────────────────────┐
│  Tier 0 — Wari kernel nativo (~8 KLOC Rust)        │
│  • Sv39 paging, traps, scheduler                   │
│  • Capability primitive (estática Phase 0-1)       │
│  • Loader + validador + signature check            │
└────────────────────────────────────────────────────┘
                       ↓
┌────────────────────────────────────────────────────┐
│  RISC-V silicon (VisionFive 2 hoy, custom mañana)  │
└────────────────────────────────────────────────────┘
```

**Notas para Gustavo (60s):**

*"Tres tiers. La diferencia con Linux es brutal: en Linux, drivers viven
en el kernel — un bug de driver compromete TODO el sistema. En Wari, los
drivers son módulos WASM firmados que corren en sandbox. Driver de red
con bug = se mata ese driver, se respawnea, kernel intacto, otros
tenants intactos.*

*Tier 1 es donde corre el código del cliente. Doble sandbox: WASM
arriba, MMU abajo. Para escapar, un atacante necesita romper LAS DOS
capas Y la capa de capacidades. Ese tipo de aislamiento por construcción
es el feature."*

---

## Slide 7 — Por qué encaja en EPAM Garage

> **Wari es exactamente el tipo de research project que arma posición
> de mercado para el Garage.**

```
EPAM Garage MX necesita:           Wari ofrece:

  Diferenciador técnico real    →  Categoría nueva (sovereign WASM-OS)
  Story de soberanía LATAM      →  AGPL + RISC-V + sin telemetría
  IP publicable / paper-able    →  Custom RISC-V extension (Zwari)
  Hook para deals gubernamentales→ Auditabilidad como sello
  Talento que se queda          →  Open source upstream + reputación
```

**Notas para Gustavo (60s):**

*"No te estoy proponiendo que el Garage compre Wari, ni que se vuelva
producto EPAM. Wari se queda mío en GitHub personal, AGPL, upstream
libre. Lo que te propongo es que el Garage tenga un research track
formal alrededor de soberanía digital LATAM, y Wari es el primer
artefacto técnico de ese track.*

*Eso te da: papers para conferencias (CARRV, ASPLOS), credibilidad
técnica que ningún consultor LATAM tiene hoy, y un gancho concreto para
deals donde 'soberanía' es palabra que ya estás usando en sales pero
sin nada concreto detrás."*

---

## Slide 8 — Roadmap (la trayectoria que importa)

```
Phase 1b · 8 semanas
  ├─ Capacidades + scheduler + IPC
  ├─ Driver Tier-2 de red
  └─ Demo: cluster 3 VF2 + REST API + chaos test

Phase 2 · 3-6 meses
  └─ Swap intérprete: wasmi → Wasm3 portado
       · 4-10× perf gain, TCB IGUAL o más chico
       · NO JIT (rompería tesis de auditabilidad)

Phase 3 · 12-24 meses · EL MOAT
  └─ Zwari — RISC-V custom extension para WASM
       · Hardware acceleration sin W^X violation
       · Paper publicable, reproducible
       · FPGA prototype primero, ASIC después

Phase 4 · 3-5 años
  └─ ASIC tapeout — silicio soberano
       · Posible partnership con gobierno LATAM
       · Categoría única en el mercado mundial
```

**Notas para Gustavo (90s — la slide más importante):**

*"Aquí está la jugada que ningún otro WASM runtime hace. Todos los demás
— wasmtime, wasmer, WAMR — para alcanzar performance se van a JIT.
JIT son 250 mil líneas de Cranelift que rompen mi tesis de TCB chico,
abren superficie de ataque (W^X violations), y matan la auditabilidad.*

*Yo voy por el camino largo: intérprete optimizado primero (Phase 2,
ya identificamos Wasm3 como candidato — es 4-10× más rápido que lo que
usamos hoy y MÁS chico), después aceleración por hardware con extensión
custom RISC-V, después silicio propio.*

*Cada paso preserva la tesis de auditabilidad. Cada paso es publicable.
El estado final es algo que no existe en ningún otro lado del mundo:
WASM ejecutándose nativamente en silicio que diseñamos.*

*Y para LATAM eso es la jugada de soberanía completa: software
abierto + hardware abierto + silicio propio. No depender de Intel, no
depender de ARM, no depender de AWS."*

---

## Slide 9 — Lo que pediría del Garage

> **No te pido budget hoy. Te pido tres cosas, en orden de valor:**

**(1) Acceso a un cliente — lo único irreemplazable.**

*Una intro a UN cliente del portfolio LATAM con dolor real de
soberanía digital. Banca mediana, ministerio, telco regulada,
salud — el que sea. Yo llevo el demo cuando esté listo. Ellos
validan o rechazan la propuesta. Esto cierra el loop.*

**(2) Endorsement informal del Garage.**

*Permiso para mencionar "research track del EPAM Garage MX" en
conversaciones con potenciales colaboradores académicos
(Tec, UNAM, ITAM). No necesito comunicado formal — solo poder
decir que el Garage está al tanto y le interesa.*

**(3) Recursos materiales (opcional, último).**

*Si después de ver el demo del cluster te convence, 4 horas/semana
de un ingeniero del Garage durante 8 semanas + presupuesto para
2 VF2 más (~$300 USD). Pero esto es nice-to-have, no
make-or-break.*

**Notas para Gustavo (45s):**

*"Lo más valioso que puedes darme es un cliente concreto. Sin un
cliente, esto se queda como research bonito sin validación de mercado.
Con un cliente, aunque diga 'no me interesa', aprendo qué le falta a
Wari para SER interesante. Esa información es oro.*

*Lo segundo es endorsement informal — solo poder decir 'el Garage está
al tanto.' Eso me abre puertas con academia y comunidad open-source que
sin ese sello no se abren.*

*Lo tercero — recursos — solo si después de un segundo demo te terminas
de convencer. No te pido nada hasta entonces."*

---

## Slide 10 — Próximo paso

> **Si te suena, dos opciones — tú escoges:**

**(a) Demo técnico en 4 semanas.**
Tú + yo, oficina del Garage. Te enseño Phase 1b corriendo: cluster
de 3 VF2, REST API real, chaos test (jalo el cable de un nodo en
vivo, tráfico continúa). Salida: decides si vale escalarlo a la
opción (b).

**(b) Pitch a stakeholders en 8 semanas.**
Tú + 2-3 personas que escojas (head of cloud modernization, head
de un vertical, cliente potencial). Demo completo + reference
architecture + propuesta de research track formal con cliente
piloto identificado.

**Mi voto: (a) primero, (b) después solo si (a) prende.**

```
Repo:    github.com/westerngazoo/Wari
Demo:    yo lo monto en el Garage cuando me digas
Tiempo:  podemos arrancar esta semana
```

**Notas para Gustavo (30s — el cierre):**

*"No te estoy pidiendo decisión hoy. Te estoy pidiendo que me digas si
te suena la categoría — sovereign WASM-OS para LATAM — y si vale la
pena que lo armemos en serio. Si dices que no, sin problema, sigue
siendo mi proyecto personal y entendí algo importante. Si dices que sí,
arrancamos con un demo técnico en 4 semanas y lo vamos viendo."*

*Termina con: "¿qué opinas?"*

*Después: cállate. Deja que hable. La conversación es el producto.*

---

## Apéndice — preguntas que probablemente te haga, con respuestas preparadas

**P1: "¿Y la performance vs Linux?"**

R: *Honestamente, hoy 10-50× más lento por ser intérprete puro. Phase 2
lo lleva a 3-10× más lento. Phase 3 (custom extension) a 1.2-2×. JIT no
es el camino — rompe la tesis. Para los workloads donde Wari debe ganar
(soberanía + auditabilidad) ese trade-off es deliberado y vendible.*

**P2: "¿IP? ¿De quién es esto si lo metemos al Garage?"**

R: *Wari es mío, AGPL, GitHub personal, fuera de horas EPAM. Si el
Garage hace research track alrededor, la propiedad upstream se queda
mía y libre — eso es no-negociable, es la postura del manifiesto. EPAM
puede hacer fork interno, contribuciones upstream, consultoría, o
licencia comercial dual si después tiene sentido. Pero Wari core
upstream no se cierra.*

**P3: "¿Y si EPAM corporate dice que no?"**

R: *Sigue siendo mi proyecto. Busco sponsor en otro lado — academia,
fundación, gobierno LATAM directo. La diferencia es que pierdo el canal
EPAM. Esto es por eso que es importante saberlo temprano.*

**P4: "¿Cuánto tiempo te toma esto solo?"**

R: *A ritmo actual (fines de semana), Phase 1b en 8 semanas, Phase 2
en 6 meses. Con apoyo (4 hrs/sem de un ingeniero), Phase 1b en 4
semanas, Phase 2 en 3 meses. Phase 3 (Zwari) requiere FPGA + tiempo
de hardware design — eso sí es trabajo full-time o equipo.*

**P5: "¿Quién más está haciendo esto?"**

R: *Nadie en LATAM. Globalmente: Bytecode Alliance (wasmtime, sobre
Linux, no kernel), Cosmonic / Fermyon (managed cloud, no soberano),
Hubris de Oxide (Rust microkernel, no WASM-native). Wari es el único
WASM-native + RISC-V + AGPL + soberanía-LATAM-positioned.*

**P6: "¿Y si esto se vuelve grande, lo vendes?"**

R: *AGPL impide que se cierre. Lo que sí se puede vender: licencia
comercial dual para clientes que no pueden adoptar AGPL, consultoría
de despliegue, certificación de hardware. El upstream se queda libre.*

---

## Checklist pre-junta para Gustavo

- [ ] Ensayar las 10 slides una vez en voz alta — tiempo target: 8-10 min máximo
- [ ] Tener laptop con repo público abierto en GitHub para ilustrar
- [ ] Tener video de 90 seg del boot grabado como backup (por si no hay internet en el Garage)
- [ ] Decidir respuesta a P2 (IP) ANTES — esa es la pregunta crítica y no quieres improvisar
- [ ] No llevar VF2 físico al café (innecesario, abrumador para charla informal)
- [ ] Llevar la foto del primer boot impresa o en celular como abridor visual

## Lo que NO debes hacer

- **No vendas.** Eres su empleado, no eres vendor. Si suena a pitch
  comercial, pierde la confianza
- **No prometas timelines agresivos.** Ofrece "8 semanas con apoyo,
  más sin." Honestidad de timeline > optimismo
- **No le digas qué cliente debe darte.** Que él proponga. Tu propuesta
  es el problema técnico, no el go-to-market
- **No le pidas que firme nada.** Es café, no contrato

## Lo que SÍ haz

- **Llega temprano.** Pide su café como lo toma él
- **Habla del Garage primero, de Wari segundo.** Pregúntale cómo va
  el portfolio antes de meter tu agenda
- **Cuando termines de presentar, pregunta y cállate.** El silencio
  incómodo es bueno — fuerza pensamiento real
- **Pide la siguiente reunión antes de irte.** "¿Te parece si te mando
  el demo en 4 semanas?" Cierra con calendario
