// SPDX-License-Identifier: AGPL-3.0-only
//! Arithmetic of the ADR-004 step-2 measurement gate: turning timer
//! ticks into nanoseconds, reading the ASID width back from `satp`,
//! and the 20 µs tenant-switch budget the design is reviewed against.
//!
//! The kernel side (`kernel/src/umode_bench.rs`) only reads CSRs and
//! counts ticks; every number it prints goes through here.
//!
//! Pure: no `unsafe`, host-testable.

/// ADR-004: if one tenant switch costs more than this, the design is
/// reviewed before wasmi moves to U-mode (step 3).
pub const SWITCH_BUDGET_NS: u64 = 20_000;

/// Bit position of the ASID field in an Sv39 `satp`.
pub const SATP_ASID_SHIFT: u32 = 44;

/// The ASID field of an Sv39 `satp` (16 bits, `satp[59:44]`).
pub const SATP_ASID_MASK: u64 = 0xFFFF << SATP_ASID_SHIFT;

const NS_PER_SEC: u128 = 1_000_000_000;

/// Number of ASID bits the hart implements, from the `satp` read back
/// after writing all ones to the ASID field.
///
/// The privileged spec makes the implemented bits the low `ASIDLEN`
/// bits of the field, so the count of ones is `ASIDLEN` (0..=16).
///
/// ```
/// use wari_sched::bench::{asid_bits, SATP_ASID_SHIFT};
/// assert_eq!(asid_bits(0), 0);
/// assert_eq!(asid_bits(0x1ff << SATP_ASID_SHIFT), 9);
/// ```
pub fn asid_bits(satp_readback: u64) -> u32 {
    ((satp_readback & SATP_ASID_MASK) >> SATP_ASID_SHIFT).count_ones()
}

/// Average nanoseconds per iteration for `ticks` timer ticks spent on
/// `iters` iterations of a timer running at `timebase_hz`.
///
/// Returns `None` if `timebase_hz` or `iters` is 0, or the result does
/// not fit in a `u64`. Rounds down.
///
/// ```
/// use wari_sched::bench::avg_ns;
/// // 4 MHz timer: one tick is 250 ns.
/// assert_eq!(avg_ns(40_000, 4_000_000, 10_000), Some(1_000));
/// assert_eq!(avg_ns(1, 0, 1), None);
/// ```
pub fn avg_ns(ticks: u64, timebase_hz: u64, iters: u64) -> Option<u64> {
    if timebase_hz == 0 || iters == 0 {
        return None;
    }
    let ns = (ticks as u128) * NS_PER_SEC / ((timebase_hz as u128) * (iters as u128));
    u64::try_from(ns).ok()
}

/// Does a tenant switch costing `switch_ns` stay within ADR-004's budget?
pub fn switch_within_budget(switch_ns: u64) -> bool {
    switch_ns <= SWITCH_BUDGET_NS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asid_bits_counts_only_the_asid_field() {
        assert_eq!(asid_bits(SATP_ASID_MASK), 16);
        assert_eq!(asid_bits(0x7f << SATP_ASID_SHIFT), 7);
        // Mode (bits 60..64) and PPN (bits 0..44) never count.
        let mode_and_ppn = (8u64 << 60) | ((1u64 << 44) - 1);
        assert_eq!(asid_bits(mode_and_ppn), 0);
    }

    #[test]
    fn avg_ns_converts_ticks_on_both_boards() {
        // VF2: 4 MHz, 250 ns per tick.
        assert_eq!(avg_ns(4, 4_000_000, 1), Some(1_000));
        // QEMU virt: 10 MHz, 100 ns per tick.
        assert_eq!(avg_ns(25, 10_000_000, 1), Some(2_500));
        // Rounds down.
        assert_eq!(avg_ns(1, 4_000_000, 3), Some(83));
    }

    #[test]
    fn avg_ns_rejects_zero_rate_or_iterations_and_overflow() {
        assert_eq!(avg_ns(10, 0, 10), None);
        assert_eq!(avg_ns(10, 4_000_000, 0), None);
        assert_eq!(avg_ns(u64::MAX, 1, 1), None);
    }

    #[test]
    fn budget_is_inclusive_at_twenty_microseconds() {
        assert!(switch_within_budget(SWITCH_BUDGET_NS));
        assert!(switch_within_budget(0));
        assert!(!switch_within_budget(SWITCH_BUDGET_NS + 1));
    }
}
