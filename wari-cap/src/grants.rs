// SPDX-License-Identifier: AGPL-3.0-only
//! Capability grant specs and attenuation — the Supervisor's arithmetic.
//!
//! One concern, and it is pure: deciding *which* authorities a
//! runtime-loaded module actually receives. A module **requests** a
//! set of authorities; the Supervisor holds a **ceiling** — the most
//! any module of that class may hold; the module is granted their
//! intersection. Nothing here touches a CSpace, a pool, or a static —
//! it is bitset logic, host-tested, and the kernel's install path
//! consumes the result.
//!
//! ## Why intersection, not the request
//!
//! Least authority is the whole game (seL4, `docs/prior-art.md`). A
//! module cannot be trusted to ask for only what it needs — a
//! prompt-injected or malicious one asks for everything. So the grant
//! is `requested & ceiling`: the module never receives an authority
//! the ceiling withholds, no matter what it requests. Attenuation is
//! monotone — you can only ever lose bits crossing this boundary,
//! never gain them, which is the property a capability system must
//! preserve (INV-10, monotonicity, applied at the grant point).
//!
//! ## Scope
//!
//! Baseline authority (stdout + exit) is NOT in this set — every Tier-1
//! module gets it unconditionally, so representing it would invite a
//! ceiling that could accidentally withhold the ability to print or
//! exit. This set is exactly the *optional, dangerous* authorities a
//! spawn may or may not confer.

/// A set of optional Tier-1 authorities, as a bitset.
///
/// Used in two roles: what a module **requests** at spawn, and the
/// **ceiling** the Supervisor permits. [`GrantSpec::attenuate`]
/// combines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantSpec(u32);

impl GrantSpec {
    /// No optional authority — a plain tenant (stdout + exit only).
    pub const EMPTY: GrantSpec = GrantSpec(0);

    /// `EventLog` READ — observe the kernel audit stream. The guard
    /// agent's one authority. Bit 0.
    pub const EVENTLOG: GrantSpec = GrantSpec(1 << 0);

    /// A `Net` capability — open sockets, reach the network. Bit 1.
    /// A runtime-loaded module cannot yet hold this (no ceiling admits
    /// it — networked dynamic modules need an attestation the signing
    /// pipeline does not produce today), so it exists to be *requested
    /// and denied*, which is exactly the attenuation this module
    /// demonstrates.
    pub const NET: GrantSpec = GrantSpec(1 << 1);

    /// Build from raw bits (e.g. decoded from a manifest field).
    /// Unknown bits are preserved by the type but withheld by every
    /// real ceiling, so a module declaring a future authority this
    /// kernel does not understand is attenuated to nothing extra
    /// rather than mis-granted.
    pub const fn from_bits(bits: u32) -> GrantSpec {
        GrantSpec(bits)
    }

    /// Raw bits, for encoding into an audit record.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The authorities actually granted: `self` (requested) ∩
    /// `ceiling`. The Supervisor's core operation.
    ///
    /// ```
    /// use wari_cap::grants::GrantSpec;
    /// // A module asks for EventLog + Net; the ceiling permits only
    /// // EventLog. It gets EventLog; Net is denied.
    /// let requested = GrantSpec::EVENTLOG.with(GrantSpec::NET);
    /// let ceiling = GrantSpec::EVENTLOG;
    /// assert_eq!(requested.attenuate(ceiling), GrantSpec::EVENTLOG);
    /// // You can never gain a bit crossing the boundary.
    /// assert_eq!(GrantSpec::EMPTY.attenuate(ceiling), GrantSpec::EMPTY);
    /// ```
    pub const fn attenuate(self, ceiling: GrantSpec) -> GrantSpec {
        GrantSpec(self.0 & ceiling.0)
    }

    /// The authorities requested but withheld by the ceiling —
    /// `self` (requested) minus `granted`. Non-empty means attenuation
    /// happened; the kernel emits an audit record of exactly these
    /// bits so a guard can see what was denied to whom.
    pub const fn denied(self, granted: GrantSpec) -> GrantSpec {
        GrantSpec(self.0 & !granted.0)
    }

    /// Union — build a request from several authorities.
    pub const fn with(self, other: GrantSpec) -> GrantSpec {
        GrantSpec(self.0 | other.0)
    }

    /// Does this set include `flag`?
    pub const fn contains(self, flag: GrantSpec) -> bool {
        self.0 & flag.0 == flag.0
    }

    /// No optional authority at all.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attenuate_is_intersection_and_monotone() {
        let ceiling = GrantSpec::EVENTLOG;
        // Requesting more than the ceiling yields exactly the ceiling
        // overlap — never more.
        let req = GrantSpec::EVENTLOG.with(GrantSpec::NET);
        assert_eq!(req.attenuate(ceiling), GrantSpec::EVENTLOG);
        // Requesting exactly the ceiling passes through.
        assert_eq!(GrantSpec::EVENTLOG.attenuate(ceiling), GrantSpec::EVENTLOG);
        // Requesting nothing yields nothing.
        assert_eq!(GrantSpec::EMPTY.attenuate(ceiling), GrantSpec::EMPTY);
        // Monotonicity: the granted set is always a subset of BOTH
        // request and ceiling — you cannot gain a bit here.
        for r in 0u32..8 {
            for c in 0u32..8 {
                let g = GrantSpec::from_bits(r).attenuate(GrantSpec::from_bits(c));
                assert_eq!(g.bits() & !r, 0, "granted a bit not requested");
                assert_eq!(g.bits() & !c, 0, "granted a bit above the ceiling");
            }
        }
    }

    #[test]
    fn denied_names_exactly_the_withheld_bits() {
        let ceiling = GrantSpec::EVENTLOG;
        let req = GrantSpec::EVENTLOG.with(GrantSpec::NET);
        let granted = req.attenuate(ceiling);
        assert_eq!(req.denied(granted), GrantSpec::NET);
        // Nothing withheld when the request is within the ceiling.
        assert!(GrantSpec::EVENTLOG.denied(GrantSpec::EVENTLOG).is_empty());
    }

    #[test]
    fn unknown_future_bit_is_attenuated_away_by_a_real_ceiling() {
        // A module declares bit 31 (some authority this kernel does not
        // know). No real ceiling sets it, so it is withheld — a
        // forward module cannot smuggle authority past an old kernel.
        let future = GrantSpec::from_bits(1 << 31);
        assert!(future.attenuate(GrantSpec::EVENTLOG).is_empty());
    }

    #[test]
    fn contains_and_empty() {
        let s = GrantSpec::EVENTLOG.with(GrantSpec::NET);
        assert!(s.contains(GrantSpec::EVENTLOG));
        assert!(s.contains(GrantSpec::NET));
        assert!(!GrantSpec::EVENTLOG.contains(GrantSpec::NET));
        assert!(GrantSpec::EMPTY.is_empty());
        assert!(!GrantSpec::EVENTLOG.is_empty());
    }
}
