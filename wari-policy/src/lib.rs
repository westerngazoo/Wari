// SPDX-License-Identifier: AGPL-3.0-only
//! Wari — Executor policy engine (Lane B / B3).
//!
//! The **deterministic, non-LLM gate** that mediates every action the
//! AI-OS assistant's Planner proposes (see `docs/ai-os-assistant-design.md`
//! §5). The Planner's judgment is untrusted (prompt injection is assumed
//! structural), so a fully-subverted Planner must still be unable to
//! cause an effect its capabilities don't allow. This module is the pure
//! core of that boundary: it takes a [`Request`] (facts the Executor
//! established — never fields the LLM filled directly) plus the task
//! [`Budget`], and returns a [`Decision`] via a fixed, ordered gauntlet.
//!
//! Pure logic: no `unsafe`, no allocation, no I/O — host-testable, and
//! small enough to sit in the Executor's trusted core (the thing that is
//! verifiable, unlike the model). The kernel/executor wires capabilities,
//! taint labels, and the out-of-band confirm channel around it; this
//! file only decides *allow / deny / needs-confirmation*.

#![cfg_attr(not(test), no_std)]

/// Consequence class of an action — the axis the policy gates on. The
/// Executor classifies each action type; the LLM does not get to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consequence {
    /// Read-mostly and reversible (e.g. a scoped read, an inference).
    Benign,
    /// Meaningful but reversible (e.g. a write to owned state).
    Consequential,
    /// High-consequence / hard-to-undo: delete, network egress, spend,
    /// spawn, or capability grant. Always requires out-of-band authority.
    Irreversible,
}

/// One action request, as **facts the Executor established** before
/// calling [`evaluate`]. None of these come straight from the Planner's
/// text — the Executor resolves them (allow-list membership, capability
/// scope, taint provenance), which is what keeps the decision trustworthy
/// even when the Planner is compromised.
#[derive(Debug, Clone, Copy)]
pub struct Request {
    /// Opaque action-type id (the Executor's action table indexes this).
    pub action_id: u32,
    /// Consequence class the Executor assigned to `action_id`.
    pub consequence: Consequence,
    /// Is `action_id` permitted for THIS task at all? (default-deny.)
    pub in_allow_list: bool,
    /// Does the task's attenuated capability actually cover this action's
    /// target/range? (Enforced independently by the kernel too.)
    pub cap_covers_target: bool,
    /// Are the action's parameters derived from untrusted/tainted input
    /// (file contents, web, another tenant)? If so a benign-looking op
    /// (e.g. "email X") may be attacker-chosen and must be confirmed.
    pub tainted_params: bool,
}

/// Per-task budget — checked by [`evaluate`], advanced by the Executor on
/// an [`Decision::Allow`]. Bounds a subverted Planner from looping to
/// exfiltrate or brute-force.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Actions already spent this task.
    pub actions_used: u32,
    /// Hard ceiling for this task.
    pub actions_max: u32,
}

/// Why an action was refused outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// `action_id` is not in the task's allow-list (default-deny).
    NotInAllowList,
    /// The task's attenuated capability does not cover the target.
    CapScope,
    /// The per-task action budget is exhausted.
    RateExceeded,
}

/// Why an action needs out-of-band authority before it may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmReason {
    /// Parameters derive from tainted input — the op may be
    /// attacker-chosen even if its type is allow-listed.
    TaintedParams,
    /// The action is irreversible (delete/egress/spend/spawn/grant).
    Irreversible,
}

/// The Executor's ruling on one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Proceed (the Executor exercises the capability and advances the
    /// budget).
    Allow,
    /// Refuse outright.
    Deny(DenyReason),
    /// Requires an out-of-band authority the Planner cannot satisfy
    /// itself (a signed policy cap or a human/console confirm).
    Confirm(ConfirmReason),
}

/// Evaluate one action request against the task budget — the pure policy
/// gauntlet.
///
/// Ordered **cheapest-and-hardest-first**, short-circuiting on the first
/// gate that fires:
///   1. allow-list (default-deny) → [`DenyReason::NotInAllowList`]
///   2. capability scope → [`DenyReason::CapScope`]
///   3. rate/budget → [`DenyReason::RateExceeded`]
///   4. taint → [`ConfirmReason::TaintedParams`]
///   5. irreversibility → [`ConfirmReason::Irreversible`]
///   6. otherwise → [`Decision::Allow`]
///
/// A hard deny (1–3) outranks a confirm (4–5): if the action isn't even
/// permitted, we never surface a confirmation prompt for it. Taint is
/// checked before irreversibility so a tainted benign op is still gated.
///
/// ```
/// use wari_policy::{evaluate, Request, Budget, Consequence, Decision};
/// let ok = Request {
///     action_id: 1, consequence: Consequence::Benign,
///     in_allow_list: true, cap_covers_target: true, tainted_params: false,
/// };
/// let budget = Budget { actions_used: 0, actions_max: 8 };
/// assert_eq!(evaluate(&ok, &budget), Decision::Allow);
/// ```
#[inline]
pub fn evaluate(req: &Request, budget: &Budget) -> Decision {
    if !req.in_allow_list {
        return Decision::Deny(DenyReason::NotInAllowList);
    }
    if !req.cap_covers_target {
        return Decision::Deny(DenyReason::CapScope);
    }
    if budget.actions_used >= budget.actions_max {
        return Decision::Deny(DenyReason::RateExceeded);
    }
    if req.tainted_params {
        return Decision::Confirm(ConfirmReason::TaintedParams);
    }
    if matches!(req.consequence, Consequence::Irreversible) {
        return Decision::Confirm(ConfirmReason::Irreversible);
    }
    Decision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Request {
        Request {
            action_id: 1,
            consequence: Consequence::Benign,
            in_allow_list: true,
            cap_covers_target: true,
            tainted_params: false,
        }
    }
    const B: Budget = Budget {
        actions_used: 0,
        actions_max: 8,
    };

    #[test]
    fn happy_path_allows() {
        assert_eq!(evaluate(&base(), &B), Decision::Allow);
    }

    #[test]
    fn not_in_allow_list_is_first_and_short_circuits() {
        let mut r = base();
        r.in_allow_list = false;
        // Wins even when every later gate would also fire.
        r.cap_covers_target = false;
        r.tainted_params = true;
        r.consequence = Consequence::Irreversible;
        assert_eq!(evaluate(&r, &B), Decision::Deny(DenyReason::NotInAllowList));
    }

    #[test]
    fn cap_scope_denies_before_rate_and_confirm() {
        let mut r = base();
        r.cap_covers_target = false;
        r.tainted_params = true;
        let full = Budget {
            actions_used: 8,
            actions_max: 8,
        };
        assert_eq!(evaluate(&r, &full), Decision::Deny(DenyReason::CapScope));
    }

    #[test]
    fn rate_exceeded_denies_before_confirm() {
        let mut r = base();
        r.consequence = Consequence::Irreversible; // would otherwise Confirm
        let full = Budget {
            actions_used: 8,
            actions_max: 8,
        };
        assert_eq!(
            evaluate(&r, &full),
            Decision::Deny(DenyReason::RateExceeded)
        );
        // Boundary: used == max is exhausted; used < max is fine.
        let edge = Budget {
            actions_used: 7,
            actions_max: 8,
        };
        assert_eq!(
            evaluate(&r, &edge),
            Decision::Confirm(ConfirmReason::Irreversible)
        );
    }

    #[test]
    fn tainted_params_require_confirm_even_when_benign() {
        let mut r = base();
        r.tainted_params = true;
        assert_eq!(
            evaluate(&r, &B),
            Decision::Confirm(ConfirmReason::TaintedParams)
        );
    }

    #[test]
    fn taint_outranks_irreversibility() {
        let mut r = base();
        r.tainted_params = true;
        r.consequence = Consequence::Irreversible;
        // Taint is reported first (checked before the irreversibility gate).
        assert_eq!(
            evaluate(&r, &B),
            Decision::Confirm(ConfirmReason::TaintedParams)
        );
    }

    #[test]
    fn irreversible_requires_confirm() {
        let mut r = base();
        r.consequence = Consequence::Irreversible;
        assert_eq!(
            evaluate(&r, &B),
            Decision::Confirm(ConfirmReason::Irreversible)
        );
    }

    #[test]
    fn consequential_reversible_is_allowed() {
        let mut r = base();
        r.consequence = Consequence::Consequential;
        assert_eq!(evaluate(&r, &B), Decision::Allow);
    }
}
