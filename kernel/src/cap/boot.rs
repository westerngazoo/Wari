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
use super::types::{Cap, CapId, ObjectKind, CAP_RIGHT_READ, CAP_RIGHT_WRITE};

// ─────────────────────────────────────────────────────────────────
// Phase-1b proc-id assignments
// ─────────────────────────────────────────────────────────────────

/// Process id reserved for kernel-self / unused in Phase 1b.
pub const PROC_ID_RESERVED: u8 = 0;
/// Process id assigned to the signed Tier-2 UART driver.
pub const PROC_ID_TIER2_UART: u8 = 1;
/// Process id assigned to the first Tier-1 hello instance.
pub const PROC_ID_TIER1_HELLO: u8 = 2;
/// Process id assigned to the second Tier-1 hello instance — proves
/// CSpace isolation between two instances of the same WASM blob.
/// Phase 2+ replaces this hardcoded pair with a dynamic spawn API.
pub const PROC_ID_TIER1_HELLO_B: u8 = 3;
/// Process id reserved for the Phase-1b Tier-2 net driver
/// (VirtIO-net on QEMU; JH7110 GMAC on VF2 in Phase 1c). The cap is
/// installed by an extension to `init_root_caps` in PR Net-4 when the
/// signed net driver blob lands. PR Net-3 reserves the proc_id so
/// the host-fn closures can bake it in.
pub const PROC_ID_TIER2_NET: u8 = 4;

/// Slot index for a module's primary cap (UART receive on Tier-2,
/// stdout on Tier-1).
/// Conventional cap-slot index for the Tier-1 Net cap (PR Net-6b).
/// Tier-1 instances that hold a Net cap can call
/// `wari::net_socket_create(proto, slot_for_cap)` — the impl
/// validates the Net cap at SLOT_NET, then mints a derived
/// Socket cap into the caller's `slot_for_cap`.
pub const SLOT_NET: u8 = 2;

/// Conventional cap-slot index for the Tier-1 demo IPC endpoint
/// (Option B brick 3b). Both hello instances hold READ+WRITE caps
/// to ONE shared endpoint here, so instance A can `ipc_call` and
/// instance B can `ipc_recv`/`ipc_reply` across their isolation
/// boundary. Phase-2 proper mints per-channel endpoints with
/// asymmetric rights (caller WRITE-only, servicer READ+WRITE); the
/// symmetric demo grant is called out in the PR's security notes.
pub const SLOT_IPC: u8 = 3;

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

    // 3. Tier-1 hello (instance A, proc_id=2): send cap on
    //    uart_ipc_ep (stdout) + send cap on kernel_exit_ep (exit).
    let tier1_caps = caps_for(Tier::One, ModuleId::Tier1Hello);
    install_tier1_caps(
        cs,
        PROC_ID_TIER1_HELLO,
        &tier1_caps,
        uart_ipc_ep,
        kernel_exit_ep,
    );

    // 4. Tier-1 hello (instance B, proc_id=3): identical caps as
    //    instance A but in a separate CSpace. The two instances run
    //    sequentially under the Phase-1b scheduler; they share the
    //    same WASM blob but their cap state is isolated.
    install_tier1_caps(
        cs,
        PROC_ID_TIER1_HELLO_B,
        &tier1_caps,
        uart_ipc_ep,
        kernel_exit_ep,
    );

    // 5. Bump refcounts on the endpoints, allocate the Net pool
    //    entry for the NIC, install the Net cap in the net
    //    driver's CSpace. We re-take the pools borrow now that
    //    we're done with cspaces.
    let _ = cs;
    let pools = object_pools();

    // 5a. Allocate the NIC kernel object (Phase 1b QEMU = VirtIO,
    //     nic_kind=0; VF2 = GMAC, nic_kind=1). The driver will
    //     set initialized=true after PR Net-4b's NIC bring-up.
    #[cfg(feature = "qemu")]
    let nic_kind = 0u8;
    #[cfg(feature = "vf2")]
    let nic_kind = 1u8;
    let net_pool_idx = pools.nets.alloc(super::objects::Net::new(nic_kind))?;
    if let Some(net) = pools.nets.get_mut(net_pool_idx) {
        // 3 caps reference this Net: the driver's root cap (slot 0)
        // and one derived cap each in tier-1 hello A + B (slot
        // SLOT_NET = 2). PR Net-6b.
        net.refcount = 3;
    }

    // 5b. Install the Net cap (READ + WRITE) at the net driver's
    //     CSpace slot 0. PR Net-4b will use this to drive the NIC
    //     via wari::net_mmio_*. The driver running with no Net cap
    //     (e.g., if the alloc above OOMs) hits E_PERM on its first
    //     MMIO call, which is the safe failure mode.
    {
        let _ = pools; // drop pools borrow before re-acquiring cspaces
        let cs = cspaces();
        let net_cs = &mut cs[PROC_ID_TIER2_NET as usize];
        net_cs.slots[SLOT_PRIMARY as usize] = Cap {
            badge: 0,
            parent: CapId::ROOT,
            generation: 0,
            pool_index: net_pool_idx,
            kind: ObjectKind::Net,
            rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE,
        };
        // PR Net-6b: install Net cap into both Tier-1 hello CSpaces.
        install_tier1_net_cap(cs, PROC_ID_TIER1_HELLO, net_pool_idx);
        install_tier1_net_cap(cs, PROC_ID_TIER1_HELLO_B, net_pool_idx);
    }

    // 5b-ipc (Option B brick 3b). One shared demo IPC endpoint,
    // READ+WRITE cap at SLOT_IPC in both Tier-1 hello CSpaces —
    // the channel for the cross-tenant call/recv/reply demo.
    {
        let pools = object_pools();
        let demo_ep_idx = pools.endpoints.alloc(Endpoint::new())?;
        if let Some(ep) = pools.endpoints.get_mut(demo_ep_idx) {
            ep.refcount = 2; // one cap in each hello CSpace
        }
        let _ = pools;
        let cs = cspaces();
        for pid in [PROC_ID_TIER1_HELLO, PROC_ID_TIER1_HELLO_B] {
            cs[pid as usize].slots[SLOT_IPC as usize] = Cap {
                badge: 0,
                parent: CapId::ROOT,
                generation: 0,
                pool_index: demo_ep_idx,
                kind: ObjectKind::Endpoint,
                rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE,
            };
        }
    }
    let pools = object_pools();

    // 5c. Allocate a Notification for the NIC IRQ and bind it to
    //     the platform's NIC IRQ line. PR Net-1 landed the PLIC
    //     dispatch + bind machinery; PR Net-4c wires it up with
    //     real allocations now that the net driver actually wants
    //     to wait on them.
    let nic_notif_idx = pools
        .notifications
        .alloc(super::objects::Notification::new())?;
    if let Some(notif) = pools.notifications.get_mut(nic_notif_idx) {
        notif.refcount = 1; // the driver's cap on this notification
    }

    let _ = pools; // drop borrow before re-acquiring cspaces
    let cs = cspaces();
    let net_cs = &mut cs[PROC_ID_TIER2_NET as usize];
    // SLOT_NIC_NOTIF (= 1) is the second cap in the driver's CSpace.
    // The driver calls wari::notification_wait(1) to block on
    // packet-arrival IRQs.
    net_cs.slots[1] = Cap {
        badge: 0,
        parent: CapId::ROOT,
        generation: 0,
        pool_index: nic_notif_idx,
        kind: ObjectKind::Notification,
        rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE,
    };
    let _ = cs;

    // Bind the IRQ source. QEMU virt routes VirtIO MMIO devices to
    // PLIC IRQs starting at 1 (per the QEMU virt machine.c); the
    // 4th VirtIO MMIO device (which is where we put VirtIO-net at
    // 0x10008000) is IRQ 8. VF2 GMAC IRQ numbers come in Phase 1c.
    #[cfg(feature = "qemu")]
    let nic_irq: u32 = 8;
    #[cfg(feature = "vf2")]
    let nic_irq: u32 = 0; // sentinel; not used because vf2 driver is stub
    #[cfg(feature = "qemu")]
    {
        crate::mmio::plic::bind_irq_to_notification(nic_irq, nic_notif_idx)?;
        crate::mmio::plic::enable_irq(nic_irq, 1)?;
    }
    #[cfg(feature = "vf2")]
    {
        let _ = nic_irq;
    }

    let pools = object_pools();

    // UART ep refs: 1 for Tier-2 (if mmio_uart), plus 1 per Tier-1
    // instance (if stdout).
    let uart_refs = (tier2_caps.mmio_uart as u16) + 2 * (tier1_caps.stdout as u16);
    // Exit ep refs: 1 per Tier-1 instance (if exit).
    let exit_refs = 2 * (tier1_caps.exit as u16);
    if let Some(ep) = pools.endpoints.get_mut(uart_ipc_ep) {
        ep.refcount = ep.refcount.saturating_add(uart_refs);
    }
    if let Some(ep) = pools.endpoints.get_mut(kernel_exit_ep) {
        ep.refcount = ep.refcount.saturating_add(exit_refs);
    }

    Ok(())
}

/// Install Tier-1 stdout + exit root caps into the CSpace at
/// `proc_id`, reading the on/off flags from `tier1_caps`.
fn install_tier1_caps(
    cs: &mut [super::cspace::CSpace],
    proc_id: u8,
    tier1_caps: &super::Caps,
    uart_ipc_ep: u16,
    kernel_exit_ep: u16,
) {
    let tier1_cs = &mut cs[proc_id as usize];
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
}

/// Install the Tier-1 Net cap (PR Net-6b). Derived from the
/// driver's root Net cap — same `pool_index`, refcount-managed
/// at the pool level. Granted READ + WRITE so the calling
/// Tier-1 can socket_create / socket_close. Phase-1b grants the
/// same Net cap to every Tier-1 in the demo; Phase 2+ ties the
/// grant to a manifest-declared capability request.
pub(super) fn install_tier1_net_cap(
    cs: &mut [super::cspace::CSpace],
    proc_id: u8,
    net_pool_idx: u16,
) {
    cs[proc_id as usize].slots[SLOT_NET as usize] = Cap {
        badge: 0,
        parent: CapId::ROOT,
        generation: 0,
        pool_index: net_pool_idx,
        kind: ObjectKind::Net,
        rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE,
    };
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
fn install_root_cap(cs: &mut CSpace, slot: u8, kind: ObjectKind, pool_index: u16, rights: u8) {
    cs.slots[slot as usize] = Cap {
        badge: 0,
        parent: CapId::ROOT,
        generation: 0,
        pool_index,
        kind,
        rights,
    };
}
