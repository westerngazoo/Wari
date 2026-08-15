# Wari OS & GAPU: Arquitectura de Sistema y Metas (v2.0)
**Documento de Diseño, Restricciones Físicas y Roadmap**

> **Provenance**: authored by Gustavo Delgadillo; committed to the repo
> 2026-08-15 from the working session where the strategic order was
> decided. The engineering review of this document — including two
> factual corrections and the resulting sequencing — is
> [`gapu-fit-review.md`](gapu-fit-review.md). The original said
> "Wary OS" throughout; the project's canonical name is **Wari** (和力)
> and the spelling is corrected here by the author's decision.

---

## 1. Misión del Proyecto (El Goal Fundamental)

El objetivo de nuestra arquitectura no es competir en el terreno de las
GPUs tradicionales (álgebra lineal clásica en fp32), sino construir el
primer ecosistema de hardware y software (GAPU + Wari OS) que procese
nativamente la realidad física del universo mediante el **Álgebra
Geométrica (Álgebra de Clifford)**.

Para lograr un *datapath* combinacional puro de un solo ciclo sin
fundir el silicio por el área requerida, **abandonamos la coma flotante
(IEEE 754)**. La arquitectura completa operará sobre aritmética modular
entera en campos finitos (F_p), manteniendo intactas la equivariancia
geométrica y las propiedades topológicas fundamentales, utilizando
estrictamente la constante del círculo completo (τ) para la fase de
rotores en todo el *stack* de físicas y osciladores.

---

## 2. Arquitectura de Hardware: GAPU (Geometric Algebra Processing Unit)

### 2.1. Plataforma de Lanzamiento (Proof of Concept)

* **Hardware Objetivo:** Kria KV260 (Xilinx Zynq UltraScale+ ZU5EV).
* **Álgebra Objetivo:** Cl(1,3) (16 elementos por multivector).
* **Dimensión del Cómputo:** Producto denso completo (256 productos
  parciales).

### 2.2. Diseño Lógico y Datapath en F_p

* **Aritmética Modular:** Operaciones nativas sobre un primo de
  Mersenne para reducir operaciones a nivel de bits (shifts y sumas).

  p = 2^31 − 1

* **Consumo de DSPs:** Asignando aproximadamente 4 bloques DSP por
  producto parcial de 31 bits, el diseño requiere alrededor de 1024
  DSPs. Esto encaja en los 1248 bloques DSP disponibles en el chip
  UltraScale+.
* **Sum-Tree:** La profundidad lógica para agrupar los 16 productos
  parciales por elemento de salida es de 4 niveles binarios. Esto
  garantiza una ejecución combinacional masivamente paralela a alta
  frecuencia (objetivo > 300 MHz).

### 2.3. Estrategia de Escalamiento a Cl(1,7)

Para la eventual expansión a 256 dimensiones espaciales (65,536
productos parciales), el hardware no será denso. Implementaremos
**esparcidad por grados**:

* *Datapaths* dedicados exclusivamente al subálgebra par (rotores de
  128 componentes) o a bivectores (campos de calibre, 28 componentes).

---

## 3. Arquitectura de Software: Wari OS

El microkernel/multikernel Wari OS (Rust) actuará puramente como un
orquestador de descriptores de tareas asíncronas para módulos
WebAssembly (Wasm) en anillo cero, descargando todo el cálculo a la
GAPU.

### 3.1. Gestión de Memoria y Realidades Físicas (Zero-Copy)

El concepto de *Zero-Copy Message Passing* a través de la memoria
lineal de Wasm enfrenta severas restricciones físicas que el equipo
debe resolver:

* **Paginación Contigua:** La memoria virtual contigua de Wasm no
  garantiza contigüidad física. Sin un IOMMU nativo, Wari OS deberá
  implementar un mecanismo estricto para alojar (*allocar*) estos
  buffers en bloques de memoria físicamente contigua y pineada desde
  el arranque del subsistema GAPU.
* **Coherencia de Caché en SoC JH7110:** El bus DMA de la arquitectura
  StarFive JH7110 no es coherente con el caché L2 del núcleo RISC-V.
  El equipo del kernel debe optimizar el costo operativo de inyectar
  barreras explícitas (`flush/invalidate`) en cada transferencia de
  ida y vuelta a la GAPU para mitigar el costo de esta "copia oculta".

  > **Corrección del equipo de kernel (2026-08-15)**: la afirmación
  > anterior fue refutada en silicio para el GMAC — ver
  > [`gapu-fit-review.md`](gapu-fit-review.md) §3. La coherencia del
  > master PCIe queda por verificar experimentalmente.

### 3.2. Orquestación y Recombinación

* **Tensión del Producto Geométrico:** El producto multivectorial
  mezcla grados (ej. el choque de dos cuadrivectores afecta
  componentes escalares y bivectoriales). El bus no puede cerrarse ni
  particionarse temporalmente; Wari OS no registrará una operación
  como completa hasta que la GAPU inyecte la interrupción del
  ensamblaje total del multivector de salida.

---

## 4. Metas y Entregables (Goal Set)

> **Sequencing note (2026-08-15)**: por decisión del arquitecto, la
> GAPU se difiere al final del roadmap — el orden estratégico es
> (1) núcleo del cloud OS WASM, (2) capa de capacidades AI/agentes,
> (3) multikernel, (4) GAPU. Los hitos 2 y 3 de abajo comparten
> prerequisitos con la capa AI (allocator pineado, notification
> waits), así que el camino pasa por el mismo terreno.

1. **Hito 1: Datapath Matemático (Hardware)**
   * Sintetizar el árbol combinacional Cl(1,3) mod p=2^31−1 en el
     Kria KV260.
   * Alcanzar cierre de *timing* demostrando el producto denso en 1
     ciclo de reloj (o *pipeline* mínimo).
2. **Hito 2: Memory Allocator Contiguo (Software / Wari OS)**
   * Escribir un submódulo en Rust capaz de pre-asignar páginas
     físicamente contiguas alineadas para los buffers de Wasm.
3. **Hito 3: Ping-Pong DMA en RISC-V (Integración)**
   * Levantar un *driver* de prueba en Wari OS sobre el JH7110 que
     envíe operandos *dummy* a la GAPU, maneje el `flush/invalidate`
     del caché L2 de forma determinista, y reciba la interrupción.
4. **Hito 4: Ejecución del Rotor Completo (Full Stack)**
   * Proceso de usuario compila física de rotores Cl(1,3) en Wasm
     (calculando fase geométrica con base en τ), Wari OS orquesta el
     DMA directo, GAPU resuelve en hardware y el resultado retorna al
     espacio de usuario sin bloquear el CPU principal.
