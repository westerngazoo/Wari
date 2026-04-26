// SPDX-License-Identifier: AGPL-3.0-only
//! Boot-time root-capability construction.
//!
//! `init_root_caps` is called once from `kmain` after the trap
//! vector is installed and before the runtime loads any signed
//! WASM module. It allocates the kernel-resident endpoints that
//! Phase-1b modules need to talk to each other and seeds each
//! known module's CSpace with the correct root caps per the static
//! `caps_for(Tier, ModuleId)` table.
//!
//! ## What lands here vs. PR 3
//!
//! - **This PR (2)**: object pools live, CSpaces are populated with
//!   real `Cap` values pointing at real `Endpoint`s with non-zero
//!   refcounts. **Nothing in the existing host-fn / runtime path
//!   reads any of this data yet.** The legacy `caps.mmio_uart`-
//!   boolean check in `runtime/host_fns.rs` continues to gate the
//!   actual MMIO write. Boot output is unchanged.
//! - **PR 3**: the mint/copy/revoke/delete syscalls land, the IPC
//!   send/recv syscalls land, and `host_fns::host_mmio_write8`'s
//!   legacy check is replaced by an IPC dispatch through the
//!   uart-driver endpoint. **At that point** the cap data this PR
//!   sets up becomes the load-bearing path.
//!
//! ## Process-id assignment (Phase 1b)
//!
//! | proc_id | role                                |
//! |---------|-------------------------------------|
//! | 0       | reserved (kernel-self / unused yet) |
//! | 1       | Tier-2 UART driver                  |
//! | 2       | Tier-1 hello app                    |
//! | 3..15   | unused (Phase 2+ multi-tenancy)     |
//!
//! ## Slot layout per CSpace
//!
//! Phase 1b uses simple slot numbers; PR 3's syscall surface
//! references slots by their raw `u8` index.
//!
//! - **Tier-2 UART driver (proc_id=1)**:
//!   - slot 0: `Endpoint` cap to `uart_ipc_ep`, rights = `READ`
//!     (the driver receives sends on this endpoint)
//! - **Tier-1 hello (proc_id=2)**:
//!   - slot 0: `Endpoint` cap to `uart_ipc_ep`, rights = `WRITE`
//!     (the app sends bytes; corresponds to `caps.stdout`)
//!   - slot 1: `Endpoint` cap to `kernel_exit_ep`, rights = `WRITE`
//!     (the app sends to exit; corresponds to `caps.exit`)

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use crate::error::KernelError;

use super::cspace::CSpace;
use super::objects::Endpoint;
use super::static_caps::{caps_for, ModuleId, Tier};
use super::storage::{cspaces, object_pools};
use super::types::{
    Cap, CapId, ObjectKind, CAP_RIGHT_READ, CAP_RIGHT_WRITE,
};

// ─────────────────────────────────────────────────────────────────
// Phase-1b proc-id assignments
// ─────────────────────────────────────────────────────────────────

/// Process id reserved for kernel-self / unused in Phase 1b.
pub const PROC_ID_RESERVED: u8 = 0;
/// Process id assigned to the signed Tier-2 UART driver.
pub const PROC_ID_TIER2_UART: u8 = 1;
/// Process id assigned to the Tier-1 hello app.
pub const PROC_ID_TIER1_HELLO: u8 = 2;

/// Slot index for a module's primary cap (UART receive on Tier-2,
/// stdout on Tier-1).
const SLOT_PRIMARY: u8 = 0;
/// Slot index for the exit cap (Tier-1 only).
const SLOT_EXIT: u8 = 1;

// ─────────────────────────────────────────────────────────────────
// init_root_caps
// ─────────────────────────────────────────────────────────────────

/// Allocate kernel-resident endpoints and populate per-module
/// CSpaces with root caps.
///
/// # Contract
///
/// - **Precondition**: `cspaces()` and `object_pools()` static
///   storage is in its post-`const`-init state (i.e., zeroed /
///   default — `boot.S` has run, `kmain` has not yet loaded any
///   WASM module).
/// - **Postcondition on success**:
///   - `pools.endpoints` contains 2 allocated entries (the UART
///     IPC endpoint and the kernel-exit endpoint).
///   - `cspaces[PROC_ID_TIER2_UART]` slot 0 holds a `READ`-only cap
///     to the UART endpoint.
///   - `cspaces[PROC_ID_TIER1_HELLO]` slot 0 holds a `WRITE` cap to
///     the UART endpoint and slot 1 holds a `WRITE` cap to the
///     exit endpoint (subject to `caps_for` returning `stdout=true`
///     and `exit=true`, which the Phase-0 default does).
///   - Each endpoint's `refcount` equals the number of caps now
///     referencing it (3 in total: 1 UART receive + 1 UART send + 1
///     exit send, distributed 2/1 across the two endpoints).
/// - **Errors**:
///   - `KernelError::OutOfHandles` if the endpoint pool overflows
///     (impossible in Phase 1b given pool capacity = 64 vs. 2
///     allocations, but defensively returned).
///
/// # Why this is in `cap::boot` (not in main `boot.rs`)
///
/// `kernel/src/boot.rs` hosts the staged boot sequence (UART,
/// banner, MMU, traps, …) — those stages know nothing about
/// capabilities. `cap::boot::init_root_caps` is the
/// cap-subsystem-internal initialization, called from `kmain` as
/// one ordered stage among others but living next to the cap
/// types it manipulates.
pub fn init_root_caps() -> Result<(), KernelError> {
    let pools = object_pools();

    // 1. Allocate the kernel-resident endpoints.
    let uart_ipc_ep = pools.endpoints.alloc(Endpoint::new())?;
    let kernel_exit_ep = pools.endpoints.alloc(Endpoint::new())?;

    // Drop the borrow on `pools` so we can take a fresh borrow on
    // `cspaces` next without aliasing-related grief. (See storage.rs
    // SAFETY contract.) After this point we re-acquire `pools` via a
    // helper to bump refcounts.
    let _ = pools;
    let cs = cspaces();

    // 2. Tier-2 UART driver: receive cap on uart_ipc_ep.
    let tier2_caps = caps_for(Tier::Two, ModuleId::Tier2Uart);
    if tier2_caps.mmio_uart {
        install_root_cap(
            &mut cs[PROC_ID_TIER2_UART as usize],
            SLOT_PRIMARY,
            ObjectKind::Endpoint,
            uart_ipc_ep,
            CAP_RIGHT_READ,
        );
    }

    // 3. Tier-1 hello: send cap on uart_ipc_ep (stdout) + send cap
    //    on kernel_exit_ep (exit).
    let tier1_caps = caps_for(Tier::One, ModuleId::Tier1Hello);
    let tier1_cs = &mut cs[PROC_ID_TIER1_HELLO as usize];
    if tier1_caps.stdout {
        install_root_cap(
            tier1_cs,
            SLOT_PRIMARY,
            ObjectKind::Endpoint,
            uart_ipc_ep,
            CAP_RIGHT_WRITE,
        );
    }
    if tier1_caps.exit {
        install_root_cap(
            tier1_cs,
            SLOT_EXIT,
            ObjectKind::Endpoint,
            kernel_exit_ep,
            CAP_RIGHT_WRITE,
        );
    }

    // 4. Bump refcounts on each endpoint to reflect the caps now
    //    pointing at them. We re-take the pools borrow now that we're
    //    done with cspaces.
    let _ = cs;
    let pools = object_pools();
    let uart_refs = (tier2_caps.mmio_uart as u16) + (tier1_caps.stdout as u16);
    let exit_refs = tier1_caps.exit as u16;
    if let Some(ep) = pools.endpoints.get_mut(uart_ipc_ep) {
        ep.refcount = ep.refcount.saturating_add(uart_refs);
    }
    if let Some(ep) = pools.endpoints.get_mut(kernel_exit_ep) {
        ep.refcount = ep.refcount.saturating_add(exit_refs);
    }

    Ok(())
}

/// Install a root cap (no parent — `parent = CapId::ROOT`) into a
/// CSpace slot. Used only by boot-time root-cap construction.
///
/// Phase-1b root caps:
///   - have `parent = CapId::ROOT` (no derivation chain above them);
///   - have `generation = 0` (the slot has not yet been re-occupied);
///   - have `badge = 0` (Phase-1b does not badge the boot-time caps;
///     PR 3 mints badged children when Tier-1 sends on the
///     endpoint).
fn install_root_cap(
    cs: &mut CSpace,
    slot: u8,
    kind: ObjectKind,
    pool_index: u16,
    rights: u8,
) {
    cs.slots[slot as usize] = Cap {
        badge: 0,
        parent: CapId::ROOT,
        generation: 0,
        pool_index,
        kind,
        rights,
    };
}
