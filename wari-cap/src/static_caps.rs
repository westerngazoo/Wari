// SPDX-License-Identifier: AGPL-3.0-only
//! Phase-0 static capability table.
//!
//! Three capabilities exist in Phase 0:
//!
//! | Name              | Holder (Phase 0)              | Phase-1 generalization |
//! |-------------------|-------------------------------|------------------------|
//! | `stdout`          | Tier-1 hello app              | WASI `fd_write` cap    |
//! | `mmio_uart`       | Tier-2 UART driver            | per-MMIO-window cap    |
//! | `exit`            | Tier-1 hello app              | per-process cap        |
//!
//! The mapping from `(Tier, ModuleId)` to `Caps` is hand-written in
//! `caps_for`. When a module is added in Phase 0, it must be added both
//! to `ModuleId` and to `caps_for`'s match arms. Phase 1 replaces this
//! file with a runtime registry.
//!
//! Invariants:
//!   - INV-1 (single-hart): `Caps` is a plain value; no statics; no
//!     synchronization is required.
//!   - Caps construction is **immutable post-load**: a `Tier2Instance`
//!     receives one `Caps` value at instantiate time and never mutates it.

/// Privilege tier of a WASM instance.
///
/// Tier 1 is customer code (U-mode-conceptual; no MMIO). Tier 2 is
/// signed system code (S-mode-conceptual; can hold MMIO caps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Customer / application WASM (Tier 1).
    One,
    /// Signed system / driver WASM (Tier 2).
    Two,
}

/// Modules Phase 0 recognizes by name.
///
/// Phase 1 replaces this enum with a per-instance identifier resolved
/// from a signed manifest registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleId {
    /// The Tier-2 UART driver (this PR).
    Tier2Uart,
    /// The Tier-1 hello app (PR 6).
    Tier1Hello,
    /// The Tier-2 net driver (PR Net-4a). Cap authority lives in
    /// the new `ObjectKind::Net`, not in the legacy boolean `Caps`,
    /// so `caps_for(Tier::Two, Tier2Net)` returns `Caps::empty()`.
    Tier2Net,
}

/// Per-instance capability set.
///
/// Plain by-value struct — no allocation, no `unsafe`. Each field is a
/// boolean grant that gates a specific host function or kernel facility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// May call WASI `fd_write` against the kernel UART (kernel-printk).
    pub stdout: bool,
    /// May call `wari_mmio_write8` against the NS16550 register window.
    pub mmio_uart: bool,
    /// May call `proc_exit` (Phase 0: halts the kernel after exit).
    pub exit: bool,
}

impl Caps {
    /// All-deny constructor. Used as the default for unrecognized
    /// `(Tier, ModuleId)` pairs — refusing by default is the safer
    /// posture (R5 spirit).
    pub const fn empty() -> Self {
        Self {
            stdout: false,
            mmio_uart: false,
            exit: false,
        }
    }
}

/// Default Tier-1 capability set: stdout + exit, no MMIO.
pub const TIER1_DEFAULT_CAPS: Caps = Caps {
    stdout: true,
    mmio_uart: false,
    exit: true,
};

/// Tier-2 UART driver capability set: MMIO only.
///
/// The driver's only purpose is to push bytes to the NS16550 register
/// window; it must not depend on its own host stdout (that would be
/// circular) or call exit (drivers run for the lifetime of the boot).
pub const TIER2_UART_DRIVER_CAPS: Caps = Caps {
    stdout: false,
    mmio_uart: true,
    exit: false,
};

/// Compile-time capability lookup for Phase 0.
///
/// # Contract
///
/// - For known `(tier, module_id)` pairs, returns the per-module
///   compiled-in `Caps`.
/// - For any other pair (including a Tier mismatch), returns
///   `Caps::empty()` — refuse-by-default.
///
/// # Phase-1 note
///
/// Replaced by a registry lookup keyed by signed-manifest hash.
pub const fn caps_for(tier: Tier, module_id: ModuleId) -> Caps {
    match (tier, module_id) {
        (Tier::Two, ModuleId::Tier2Uart) => TIER2_UART_DRIVER_CAPS,
        (Tier::One, ModuleId::Tier1Hello) => TIER1_DEFAULT_CAPS,
        // Tier2Net has no legacy boolean caps — its authority is
        // the `ObjectKind::Net` cap installed by
        // `cap::boot::init_root_caps`. Returning empty here is
        // correct; the runtime cap-mediated path doesn't read this
        // anyway (PR 3b retired host-fn use of the boolean caps).
        (Tier::Two, ModuleId::Tier2Net) => Caps::empty(),
        _ => Caps::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_caps_grant_nothing() {
        let c = Caps::empty();
        assert!(!c.stdout);
        assert!(!c.mmio_uart);
        assert!(!c.exit);
    }

    #[test]
    fn tier2_uart_driver_has_only_mmio() {
        let c = caps_for(Tier::Two, ModuleId::Tier2Uart);
        assert!(c.mmio_uart);
        assert!(!c.stdout);
        assert!(!c.exit);
    }

    #[test]
    fn tier1_hello_has_stdout_and_exit_no_mmio() {
        let c = caps_for(Tier::One, ModuleId::Tier1Hello);
        assert!(c.stdout);
        assert!(c.exit);
        assert!(!c.mmio_uart);
    }

    #[test]
    fn mismatched_tier_returns_empty() {
        // Tier-1 cannot impersonate the UART driver.
        let c = caps_for(Tier::One, ModuleId::Tier2Uart);
        assert_eq!(c, Caps::empty());
        // Tier-2 cannot impersonate the hello app.
        let c = caps_for(Tier::Two, ModuleId::Tier1Hello);
        assert_eq!(c, Caps::empty());
    }
}
