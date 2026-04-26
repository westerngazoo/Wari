// SPDX-License-Identifier: AGPL-3.0-only
//! Staged boot sequence — kernel entry to first Tier-1 WASM scheduled.
//!
//! Each stage is a standalone function with documented pre- and
//! post-conditions. Reading this file top-to-bottom gives a flat
//! table of contents for the boot sequence.
//!
//! Cherry-picked template: `goose-os/kernel/src/boot.rs`. Phase 0a
//! agent populates the stages in dependency order:
//!
//!   1. `stage_uart`       — early console (allow panics to print)
//!   2. `stage_banner`     — Wari banner, hart id, build info  ← landed
//!   3. `stage_interrupts` — trap vector, PLIC, timer, SIE on
//!   4. `stage_memory`     — physical page allocator + self-test
//!   5. `stage_mmu`        — Sv39 page tables + `csrw satp`
//!   6. `stage_runtime`    — wasmi embedding + WASI host fn table
//!   7. `stage_tier1_init` — load signed .wasm, spawn as PID 1
//!
//! Staging rule: a stage may not depend on a stage below it. Stage 7
//! never returns — it hands to the scheduler.

use crate::kprintln;

/// Print the Wari boot banner — ASCII chakana + EZLTN tagline +
/// build/hart line. Called once from `kmain` immediately after
/// UART init, before any other stage runs.
///
/// # Contract
///
/// - Precondition: `stage_uart` has run; `kprintln!` is functional
///   (the NS16550 / DW8250 register window is initialized).
/// - Postcondition: ~13 lines of banner are on the wire. Caller is
///   responsible for any subsequent log lines.
/// - Errors: none. UART writes are fire-and-forget; banner loss
///   under noisy boot is tolerable (downstream stages print their
///   own status, so the boot is still observable).
///
/// # Format choice (Why/How depth)
///
/// Picked: pure-ASCII chakana (Andean stepped cross) for the art,
/// UTF-8 for the tagline.
///
/// Considered:
///   - Unicode block characters (`▄ █ ▀`) for a denser visual —
///     rejected because not every serial terminal renders them
///     consistently; the boot banner is the first thing a new user
///     sees and must work everywhere
///   - Plain text "WARI" with no art — rejected as visually weak
///     for the project's identity statement
///   - Skip the banner entirely, keep the inline `kprintln!` —
///     rejected because the boot output is the first place anyone
///     encounters the project's voice; the EZLTN line declares
///     identity before any technical content
///
/// Why this won: ASCII art renders on every 8N1 terminal regardless
/// of charset (minicom, screen, picocom, putty, raw `cat /dev/ttyS0`).
/// The chakana is a real Wari/Inca symbol that maps naturally onto
/// the project's name. The tagline uses UTF-8 (Spanish accents);
/// modern terminals decode UTF-8 by default and a terminal that
/// fails degrades only on the tagline line — the art is safe.
///
/// Cost accepted: ~13 extra UART writes at boot (negligible — the
/// VF2's UART runs at 115,200 baud, this is sub-millisecond).
pub fn stage_banner(build: &str, hart_id: usize) {
    kprintln!("");
    kprintln!("       #####");
    kprintln!("       #   #");
    kprintln!("  ##### #   # #####");
    kprintln!("  #               #");
    kprintln!("  #     WARI      #");
    kprintln!("  #               #");
    kprintln!("  ##### #   # #####");
    kprintln!("       #   #");
    kprintln!("       #####");
    kprintln!("");
    kprintln!("  Soberanía tecnológica, tierra y libertad.");
    kprintln!("                                    — EZLTN");
    kprintln!("");
    kprintln!("Wari v0 build {} boot OK, hart {}", build, hart_id);
}
