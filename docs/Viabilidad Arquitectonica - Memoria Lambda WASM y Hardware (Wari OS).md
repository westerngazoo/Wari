# **Documento de Viabilidad Arquitectónica: Gestión de Memoria Basada en Cálculo Lambda y Lógica Lineal (Software WASM a Silicio Híbrido)**

---

**Proyecto Objetivo:** Wari OS (Microkernel WASM-Nativo y Arquitectura Distribuida de Procesos/Actores)  
**Fecha:** Octubre 2026  
**Enfoque:** Sistemas de Tipos Subestructurales, Redes de Interacción de Lafont, Geometría de la Interacción (GoI) y Aceleración en Hardware (FPGA / Señal Mixta).

## ---

**1\. Fundamento Matemático General**

---

La seguridad de memoria (*memory safety*) se divide formalmente en dos propiedades ortogonales: **seguridad espacial** y **seguridad temporal**. Si modelamos la memoria como una función parcial del conjunto de direcciones 𝒜 hacia los valores 𝒱, y a cada bloque asignado *b* le asociamos un intervalo espacial \[base(b), base(b) \+ size(b)) y un intervalo temporal de validez \[talloc(b), tfree(b)\], un acceso mediante un puntero *p* derivado de *b* en el instante *t* es seguro si y solo si:

\$\$ \\text{Safe}(p, t) \\iff \\big( \\text{base}(b) \\le \\text{addr}(p) \< \\text{base}(b) \+ \\text{size}(b) \\big) \\land \\big( t\_{\\text{alloc}}(b) \\le t \\le t\_{\\text{free}}(b) \\big) \$\$

En un microkernel WASM-nativo donde cada proceso es una instancia de WebAssembly ejecutándose sobre un hilo con aislamiento espacial estricto:

\$\$ \\mathcal{M}\_i \\cap \\mathcal{M}\_j \= \\emptyset \\quad \\forall i \\neq j \$\$

Para evaluar cada paradigma sin recolector de basura trazador (GC), definimos el balance espaciotemporal de la memoria bajo la invariante de conservación de nodos en una reducción β o interacción α ⋈ β:

$$\Delta M(t)={N}_{alloc}(t)-{N}_{free}(t)$$

Exigimos que toda liberación 𝒩free(t) esté acoplada determinísticamente al paso de reducción algebraica en 𝒪(1) o al cierre léxico de una región estática.

## ---

**2\. Matriz de Viabilidad Arquitectónica**

---

| Vector Arquitectónico | Nivel de Madurez (TRL) | Complejidad de Implementación | Latencia de Reducción / Memoria | Overhead en Memoria Lineal WASM | Viabilidad a Corto/Mediano Plazo   |
| :---- | :---- | :---- | :---- | :---- | :---- |
| **V1. Software: Perceus / FBIP** *(Koka / Lean 4 → C → WASM)* | **TRL 8-9** (Producción / Estable) | **Baja \- Media** (Integración de Toolchain y FFI/ABI) | \~2 \- 5 ns por nodo (Mutación *in-place* en caché L1) | **Mínimo** (1 palabra para contador RC por bloque) | **Inmediata (Alta)** Ideal para el *userland* actual del OS. |
| **V2. Software: Regiones \+ Tipos Lineales** *(MLKit / Idris 2 QTT)* | **TRL 7-8** (Compiladores maduros de nicho) | **Media** (Requiere modelar *Session Types* e IPC) | \~1 ns (Avance de puntero de pila en región ρ) | **Cero** en datos lineales (ρ=1); fragmentación interna en arenas. | **Alta** Excelente para drivers, protocolos IPC y servidores del kernel. |
| **V3. VM Grafo: Redes de Interacción** *(HVM / Custom Reductor en WASM)* | **TRL 6-7** (Probado en paralelo, ecosistema joven) | **Media \- Alta** (Escribir el runtime reductor en Rust/C++23) | \~5 \- 15 ns por par activo en CPU (Limitado por caché) | **Constante** (Celdas fijas de 128 bits en un arreglo plano). | **Alta** Permite migrar sub-grafos enteros entre procesos sin serializar. |
| **V4. Hardware Digital: RPU en FPGA** *(Interaction Combinators en RTL)* | **TRL 4-5** (Prototipos / Síntesis RTL directa) | **Alta** (Diseño SystemVerilog, arbitraje BRAM y DMA) | **1 ciclo de reloj** (\~2 \- 4 ns a 250-500 MHz) con GC en 𝒪(1) | **Cero en Host** (El grafo vive en la SRAM/BRAM dedicada de la FPGA). | **Media \- Alta** Totalmente realizable en FPGAs comerciales. |
| **V5. Hardware Analógico / Señal Mixta** *(GoI Continua / Crossbar RC)* | **TRL 2-3** (Investigación física / Simulación SPICE) | **Muy Alta** (Ruido térmico, *parasitics*, diseño de PCB/ASIC) | **\< 1 ns** (Asentamiento electromagnético continuo) | **Cero Absoluto** (El estado vive en fase/frecuencia o tensiones nodales). | **Baja (Experimental)** Viable como coprocesador híbrido discreto tras simular en SPICE. |

## ---

**3\. Análisis Técnico Profundo por Vector**

### ---

**Vector 1: Evaluación Funcional In-Place (Perceus / Koka / Lean 4\)**

> * **Mecanismo Matemático (FBIP \- Functional But In-Place):** El compilador inserta instrucciones precisas de conteo de referencias basadas en análisis de pertenencia estático. Cuando una función consume un constructor *C(x1, ..., xn)* y produce otro del mismo tamaño *C'(y1, ..., yn)*, el compilador evalúa la unicidad del recurso:

\$\$ \\text{RC}(x) \= 1 \\implies \\text{Reutilizar la dirección física } \\text{addr}(x) \\text{ en el mismo ciclo de reloj} \$\$

> * **Integración con el Microkernel WASM:** Los compiladores de Koka y Lean 4 emiten código C limpio libre de dependencias complejas si se mapea el asignador sobre la memoria lineal de WASM. Las llamadas al sistema del microkernel (IPC entre procesos/hilos) se exponen como **Efectos Algebraicos Tipados**.

### **Vector 2: Sistemas de Tipos Cuantitativos (QTT) y Cálculo de Regiones**

> * **Teoría de Tipos Cuantitativa (Idris 2):** Cada variable en el contexto de tipado lleva anotada una multiplicidad ρ ∈ {0, 1, ω}:

$$\Gamma ,x{:}_{\rho }T\vdash e:U$$

> * Cuando ρ \= 0, el término se borra en tiempo de compilación (*zero-cost abstraction*). Cuando ρ \= 1, el recurso se usa exactamente una vez, permitiendo modelar **Session Types** para canales IPC infalibles entre hilos WASM.  
> * **Inferencia de Regiones de Tofte-Talpin (MLKit):** Anota cada expresión con una región léxica ρ y libera arenas enteras en 𝒪(1) al salir del alcance mediante restauración directa del puntero de pila.

### **Vector 3: Redes de Interacción en Software sobre Memoria Lineal WASM**

> * **Mecanismo Matemático:** Todo término del Cálculo Lambda se traduce a un grafo de **Combinadores de Interacción de Lafont** (γ Constructor, δ Duplicador, ε Borrador). La reducción ocurre exclusivamente cuando dos puertos principales colisionan:

\$\$ \\gamma(a, b) \\bowtie \\gamma(c, d) \\longrightarrow (a \\sim c) \\land (b \\sim d) \\quad \\text{(Aniquilación: libera 2 nodos en } \\mathcal{O}(1)\\text{)} \$\$

> * **Arquitectura en WASM:** La memoria lineal de cada proceso se divide en un NodePool (celdas alineadas de 128 bits) y un RedexBag (cola de pares activos de 64 bits).  
> * **Migración Zero-Copy entre Hilos WASM:** Debido a la propiedad de **Confluencia Fuerte**, cualquier sub-grafo puede cortarse y copiarse a la memoria lineal de otro proceso WASM aplicando un desplazamiento lineal de direcciones:

\$\$ P'\_{\\text{nuevo}} \= (\\text{Tag}(P) \\ll 60\) \\mid (\\text{Addr}(P) \+ \\Delta\_{\\text{offset}}) \$\$

### **Vector 4: Procesador Digital de Reducción en FPGA (RPU)**

> * **Codificación en Silicio:** Cada puerto de 32 bits se codifica como:

\$\$ P \= \\underbrace{\\text{Tag } \\tau}\_{\\text{3 bits}} \\parallel \\underbrace{\\text{Slot } s}\_{\\text{1 bit}} \\parallel \\underbrace{\\text{Dirección } \\mathcal{A}}\_{\\text{28 bits}} \$\$

> * **Hardware Free-List Stack (GC en 1 Ciclo):** Cada banco de memoria BRAM de doble puerto se acopla a una pila en hardware gobernada por el registro puntero FSP:  
  * *Aniquilación (γ ⋈ γ o δ ⋈ δ):* Conecta los cables auxiliares y devuelve ambas direcciones físicas a la pila en el mismo flanco de reloj:

$$FS{P}_{t+1}=FS{P}_{t}+2$$

> *   
  * *Conmutación (γ ⋈ δ):* Reutiliza las 2 celdas actuales y extrae únicamente 2 celdas adicionales de la pila libre:

$$FS{P}_{t+1}=FS{P}_{t}-2$$

#### **Referencia RTL en SystemVerilog (Unidad de Ejecución de Aniquilación)**

module interaction\_pe (  
    input  logic        clk,  
    input  logic        rst\_n,  
    input  logic        redex\_valid,  
    input  logic \[31:0\] port\_a, // {tag\_a\[2:0\], slot\_a\[0\], addr\_a\[27:0\]}  
    input  logic \[31:0\] port\_b, // {tag\_b\[2:0\], slot\_b\[0\], addr\_b\[27:0\]}  
    input  logic \[63:0\] node\_a\_aux,   
    input  logic \[63:0\] node\_b\_aux,  
    output logic        free\_push\_en,  
    output logic \[27:0\] free\_addr\_0,  
    output logic \[27:0\] free\_addr\_1,  
    output logic        new\_link\_valid,  
    output logic \[63:0\] link\_pair\_0,  
    output logic \[63:0\] link\_pair\_1  
);  
    typedef enum logic \[2:0\] {  
        TAG\_VAR \= 3'b000,  
        TAG\_CON \= 3'b001, // Constructor / Lambda (Gamma)  
        TAG\_DUP \= 3'b010, // Duplicador (Delta)  
        TAG\_ERA \= 3'b011  // Borrador algebraico (Epsilon)  
    } tag\_t;

    tag\_t tag\_a, tag\_b;  
    assign tag\_a \= tag\_t'(port\_a\[31:29\]);  
    assign tag\_b \= tag\_t'(port\_b\[31:29\]);

    always\_ff @(posedge clk or negedge rst\_n) begin  
        if (\!rst\_n) begin  
            free\_push\_en   \<= 1'b0;  
            new\_link\_valid \<= 1'b0;  
        end else if (redex\_valid) begin  
            if (tag\_a \== tag\_b && (tag\_a \== TAG\_CON \|\| tag\_a \== TAG\_DUP)) begin  
                new\_link\_valid \<= 1'b1;  
                link\_pair\_0    \<= {node\_a\_aux\[63:32\], node\_b\_aux\[63:32\]};  
                link\_pair\_1    \<= {node\_a\_aux\[31:0\],  node\_b\_aux\[31:0\]};  
                free\_push\_en   \<= 1'b1;  
                free\_addr\_0    \<= port\_a\[27:0\];  
                free\_addr\_1    \<= port\_b\[27:0\];  
            end else begin  
                free\_push\_en   \<= 1'b0;  
                new\_link\_valid \<= 1'b0;  
            end  
        end else begin  
            free\_push\_en   \<= 1'b0;  
            new\_link\_valid \<= 1'b0;  
        end  
    end  
endmodule

### **Vector 5: Coprocesador Analógico / Señal Mixta (Geometría de la Interacción)**

> * **Fórmula de Ejecución de Girard en el Dominio Continuo:** Un término Lambda se modela mediante un operador unitario *U* y una matriz de retroalimentación σ correspondiente a las aplicaciones β (regla *Cut*):

$$Ex(U,\sigma )={\pi }_{out}\cdot U\cdot \left({\sum\limits_{k=0}^{\infty }(\sigma U{)}^{k}}\right)\cdot {\pi }_{in}={\pi }_{out}\cdot U(I-\sigma U{)}^{-1}\cdot {\pi }_{in}$$

> * **Física del Dispositivo:** El término (I \- σU)\-1 se resuelve físicamente en picosegundos/nanosegundos mediante la estabilización de corrientes de Kirchhoff en una red de conmutadores CMOS analógicos y amplificadores de transconductancia (OTAs).  
> * **Relajación Lagrangiana Continua:** Para redes dinámicas sobre matrices memristivas/capacitivas, la reducción sigue el gradiente de un potencial de energía 𝒱(V):

$$C\frac{d{V}_{ij}}{dt}=-\frac{\partial V(V)}{\partial {V}_{ij}}$$

> * **Límite Termodinámico y Retardo de Elmore:** La acumulación de ruido térmico de Johnson-Nyquist y la capacitancia parásita *Cp* limitan la profundidad de cadenas sin regeneración:

$${v}_{n}=\sqrt{4{k}_{B}TR\Delta f},{\tau }_{Elmore}\approx \sum\limits_{k=1}^{M}{R}_{on}\cdot {C}_{p}\cdot k={R}_{on}{C}_{p}\frac{M(M+1)}{2}$$

> * **Mitigación Híbrida:** Acotar la evaluación analógica a vecindades locales (M ≤ 8 nodos) controladas por la FPGA y cuantizar las salidas con convertidores ADC rápidos (Flash ADC) de vuelta a la representación digital.

## ---

**4\. Roadmap de Ingeniería en 4 Fases**

### ---

**Fase 1 (Meses 1 \- 2): Motor de Reducción en Software y ABI para WASM**

> * **Objetivo:** Ejecutar actores funcionales estáticos en el microkernel sin GC, con paso de mensajes de copia cero.  
> * **Hito 1.1 (Definición del IR Binario):** Especificar el formato en memoria de 128 bits por nodo (NodeCell) y 64 bits por puerto (Port) alineado a páginas WASM de 64 KB.  
> * **Hito 1.2 (Micro-Runtime Freestanding en Rust/C++23):** Implementar el evaluador de Redes de Interacción (no\_std / sin STL) compilable a un binario .wasm menor a 8 KB.  
> * **Hito 1.3 (Integración con Koka / Frontend Funcional):** Configurar el pipeline de compilación desde Koka hacia WASM usando Perceus para rutas secuenciales y combinadores para rutas paralelas.  
> * **Hito 1.4 (IPC de Sub-grafos en el Kernel):** Implementar la syscall sys\_net\_migrate(target\_pid, root\_port) para transferir sub-árboles funcionales entre memorias lineales de hilos WASM.

### **Fase 2 (Meses 3 \- 4): Simulación Exacta de Ciclos y Compilador de Grafo**

> * **Objetivo:** Validar la arquitectura paralela y demostrar formalmente la ausencia de fugas de memoria y colisiones antes de sintetizar hardware.  
> * **Hito 2.1 (Traductor Lambda-a-Lafont):** Implementar el traductor de Cálculo Lambda Tipado (con cajas lineales) hacia los combinadores (γ, δ, ε).  
> * **Hito 2.2 (Simulador en Verilator):** Conectar el modelo SystemVerilog de la RPU y su *Free-List Stack* mediante DPI-C al runtime del sistema operativo.  
> * **Hito 2.3 (Benchmarking de Memoria):** Medir la tasa de reciclaje de nodos por ciclo (Δℳ) y optimizar el particionamiento de bancos de memoria.

### **Fase 3 (Meses 5 \- 7): Síntesis en FPGA y Acoplamiento con el OS**

> * **Objetivo:** Descargar la reducción de términos lambda desde los hilos WASM hacia silicio reconfigurable real.  
> * **Hito 3.1 (Síntesis del Core RPU):** Sintetizar un clúster de 4 a 8 unidades interaction\_pe operando sobre bancos BRAM de doble puerto con arbitraje libre de bloqueos.  
> * **Hito 3.2 (Interfaz de Bus AXI4-Stream / RoCC):** Conectar el clúster RPU al procesador principal vía DMA mapeado en memoria o como instrucciones extendidas (custom0 / custom1) en RISC-V.  
> * **Hito 3.3 (Driver Zero-Copy en el Microkernel):** Suspender el hilo WASM durante la transferencia DMA y despertarlo por interrupción al alcanzar la Forma Normal en la FPGA.

### **Fase 4 (Meses 8+ / I+D): Prototipo Híbrido de Señal Mixta (GoI Analógico)**

> * **Objetivo:** Construir y caracterizar en osciloscopio una celda física de reducción continua sin reloj.  
> * **Hito 4.1 (Simulación SPICE / Ngspice):** Modelar una matriz de 8 × 8 nodos de Geometría de la Interacción con interruptores CMOS analógicos de baja resistencia (serie 74HC4066 / ADG7xx) y OTAs.  
> * **Hito 4.2 (Verificación de Convergencia de Kirchhoff):** Medir el tiempo de asentamiento (τsettle) del operador (I \- σU)\-1 ante un escalón de entrada.  
> * **Hito 4.3 (PCB Hija Controlada por FPGA):** Fabricar el módulo analógico controlado por los pines GPIO de alta velocidad de la FPGA y muestrear la Forma Normal con un ADC rápido.

## ---

**5\. Arquitectura de Referencia Integrada (Stack Completo)**

---

\+-----------------------------------------------------------------------+  
\| CAPA DE APLICACIÓN / ACTORES (Procesos Aislados en el OS)             \|  
\|   \[ Código en Koka / Lean 4 / DSL Funcional Estático \]                \|  
\|        \| (Compilación estática / Traducción a Combinadores)           \|  
\+--------v--------------------------------------------------------------+  
\| CAPA DE EJECUCIÓN VIRTUAL (WASM Linear Memory por Hilo)               \|  
\|   \+-------------------------------+  \+----------------------------+   \|  
\|   \| Código Secuencial Rápido      \|  \| Sub-grafos Paralelizables  \|   \|  
\|   \| \-\> Perceus RC (In-Place Mut)  \|  \| \-\> NodePool (128b cells)   \|   \|  
\|   \+-------------------------------+  \+-------------+--------------+   \|  
\+----------------------------------------------------\|------------------+  
\| MICROKERNEL WASM-NATIVO (Scheduler & IPC)          \| (Zero-Copy DMA)  \|  
\|   \- Aislamiento espacial por instancia WASM        \|                  \|  
\|   \- Migración de sub-grafos entre hilos (memcpy)   \|                  \|  
\+----------------------------------------------------v------------------+  
\| CAPA DE ACELERACIÓN FÍSICA (Coprocesador Hardware)                    \|  
\|   \+-----------------------------------+  \+------------------------+   \|  
\|   \| RPU Digital en FPGA (SystemVerilog)\|  \| Celda GoI Analógica    \|   \|  
\|   \| \- Reducción γ ⋈ δ en 1 ciclo      \|  \| \- Crossbar CMOS \+ OTAs \|   \|  
\|   \| \- Hardware Free-List Stack O(1)   \|  \| \- Convergencia KCL     \|   \|  
\|   \+-----------------------------------+  \+------------------------+   \|  
\+-----------------------------------------------------------------------+

## ---

**6\. Recomendación de Punto de Partida**

> 1. ---

>    **Para el Userland general del OS hoy mismo:** Adoptar **Koka** compilando a C y luego a WASM. Provee Efectos Algebraicos tipados para modelar syscalls e IPC, y el algoritmo **Perceus** garantiza manejo de memoria funcional sin GC y con mutación *in-place* sobre la memoria lineal de WASM.  
> 2. **Para el motor distribuido y el puente hacia Hardware:** Definir la especificación del **Interaction Net IR de 128 bits** en Rust o C++23 freestanding. Ese mismo layout binario en la memoria lineal de WASM servirá tanto para migrar sub-grafos vivos entre procesos por IPC como para inyectarlos por DMA al clúster de reducción en la FPGA.