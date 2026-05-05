// SPDX-License-Identifier: AGPL-3.0-only
//! Staged boot sequence — kernel entry to first Tier-1 WASM scheduled.
//!
//! Each stage is a standalone function with documented pre- and
//! post-conditions. Reading this file top-to-bottom gives a flat
//! table of contents for the boot sequence.
//!
//! Stage list:
//!
//!   1. `stage_uart`       — early console (allow panics to print)
//!   2. `stage_banner`     — Wari logo + tagline, hart id, build info  ← landed
//!   3. `stage_interrupts` — trap vector, PLIC, timer, SIE on
//!   4. `stage_memory`     — physical page allocator + self-test
//!   5. `stage_mmu`        — Sv39 page tables + `csrw satp`
//!   6. `stage_runtime`    — wasmi embedding + WASI host fn table
//!   7. `stage_tier1_init` — load signed .wasm, spawn as PID 1
//!
//! Staging rule: a stage may not depend on a stage below it. Stage 7
//! never returns — it hands to the scheduler.

use crate::kprintln;

/// Print the Wari boot banner — ASCII art + tagline + build/hart
/// line. Called once from `kmain` immediately after UART init,
/// before any other stage runs.
///
/// # Contract
///
/// - Precondition: UART MMIO init has run; `kprintln!` is functional.
/// - Postcondition: ~13 lines on the wire. Caller is responsible
///   for any subsequent log lines.
/// - Errors: none. UART writes are fire-and-forget; banner loss
///   under noisy boot is tolerable (downstream stages print their
///   own status, so the boot is still observable).
///
/// # Format choice
///
/// Pure-ASCII chakana (Andean stepped cross) for the art, UTF-8
/// for the tagline. Chosen because:
///   - ASCII art renders on every 8N1 terminal (minicom, screen,
///     picocom, putty, raw `cat /dev/ttyS0`)
///   - The chakana is a real Wari/Inca symbol — visual identity
///     anchored in the project name
///   - The tagline declares The Geese Collective's voice before
///     any technical content
///
/// Cost: ~13 extra UART writes (sub-millisecond at 115,200 baud).
pub fn stage_banner(build: &str, hart_id: usize) {
    kprintln!();
    kprintln!("       #####");
    kprintln!("       #   #");
    kprintln!("  ##### #   # #####");
    kprintln!("  #               #");
    kprintln!("  #     WARI      #");
    kprintln!("  #               #");
    kprintln!("  ##### #   # #####");
    kprintln!("       #   #");
    kprintln!("       #####");
    kprintln!();
    kprintln!("  Soberanía tecnológica, tierra y libertad.");
    kprintln!("                          — The Geese Collective");
    kprintln!();
    kprintln!("Wari v0 build {} boot OK, hart {}", build, hart_id);
}
