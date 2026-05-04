// SPDX-License-Identifier: AGPL-3.0-only
//! Capability-management syscalls — the userspace-facing surface of
//! the cap system.
//!
//! Phase 1b ships these as **WASM host functions** registered with
//! the wasmi linker, *not* as RISC-V `ecall` syscalls. The Phase-0
//! kernel has no userspace ecall path (every Wari userspace module
//! is WASM, by R7), so the host-fn registration in
//! `runtime/{host_fns,wasi}.rs` is the actual ABI carrier. The
//! `SYS_CAP_*` constants in `wari-abi` document the same surface as
//! sysnums for the day a non-WASM userspace ever appears (it
//! shouldn't, but the design contract in `docs/cap-system-design.md`
//! references them).
//!
//! All five host fns return an `i32`:
//!   - `0` on success
//!   - `E_PERM` (`-1`) on permission denial
//!   - `E_INVAL` (`-2`) on bad arguments / out-of-bounds slot
//!   - `E_NOMEM` (`-3`) on pool exhaustion (cap_lookup OOB write)
//!
//! Errno values match the existing `runtime::host_fns` convention.
//!
//! ## Why these are pure functions of `(proc_id, args)`
//!
//! The host fn closure registered for Tier-1 is shaped:
//!
//! ```text
//! linker.func_wrap("wari", "cap_mint", |_caller, ps, ts, r, b| {
//!     cap_mint_impl(PROC_ID_TIER1_HELLO, ps, ts, r, b)
//! });
//! ```
//!
//! The `proc_id` is baked in at registration time. The
//! implementation here doesn't read the wasmi `Caller` at all
//! (except `cap_lookup_impl` which writes to caller's linear
//! memory). This keeps the impl testable without a wasmi context
//! and matches the goose-os pattern in similar IPC dispatchers.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use wasmi::Caller;

use super::cspace::{CSPACE_SLOTS, MAX_PROCS};
use super::revoke::{dec_refcount, inc_refcount, revoke};
use super::storage::{cspaces, object_pools};
use super::types::{
    Cap, CapId, ObjectKind, CAP_RIGHTS_PHASE_1B_MASK, CAP_RIGHT_READ,
    CAP_RIGHT_WRITE,
};

// ─────────────────────────────────────────────────────────────────
// Errno values — match runtime::host_fns convention
// ─────────────────────────────────────────────────────────────────

/// Returned to WASM when a capability check fails.
pub const E_PERM: i32 = -1;
/// Returned to WASM when an argument is malformed or out of bounds.
pub const E_INVAL: i32 = -2;
/// Returned to WASM when a pool is full or memory write fails.
pub const E_NOMEM: i32 = -3;
/// Returned to WASM when an operation would block (no IRQ pending,
/// recv buffer empty, etc.). Phase-1b polling primitive.
pub const E_AGAIN: i32 = -4;
/// Returned to WASM when a TCP socket op is attempted on a socket
/// that is not in the connected state. Added in PR Net-2 for the
/// upcoming socket host fns; consumed by PR Net-6.
pub const E_NOTCONN: i32 = -5;
/// Returned to WASM when a TCP connect attempt is rejected by the
/// peer (RST). Added in PR Net-2; consumed by PR Net-6.
pub const E_REFUSED: i32 = -6;

// ─────────────────────────────────────────────────────────────────
// check_cap — runtime permission gate
// ─────────────────────────────────────────────────────────────────

/// Verify that process `proc_id` holds a capability of `expected_kind`
/// at `slot` with **all** of the bits in `required_rights` set.
///
/// Used by host functions on the runtime fast-path (PR 3b
/// migration) to replace the legacy `host.caps.<bool>` pattern with
/// a real cap lookup. Returns `Ok(())` on success; `Err(E_PERM)` on
/// any failure. Bounds errors collapse into `E_PERM` so userspace
/// cannot distinguish "I don't have the cap" from "I asked for a
/// nonexistent slot" — both are caller errors with the same
/// remediation (don't do that).
///
/// # Invariants
///
/// - **INV-18** (CSpace Slot Index Bounds): bounds-checks `proc_id <
///   MAX_PROCS` and `slot < CSPACE_SLOTS` before any indexing.
/// - **INV-15** (Forgery Prevention): only reads the cap; never
///   constructs one.
pub fn check_cap(
    proc_id: u8,
    slot: u8,
    expected_kind: ObjectKind,
    required_rights: u8,
) -> Result<(), i32> {
    if (proc_id as usize) >= MAX_PROCS {
        return Err(E_PERM);
    }
    if (slot as usize) >= CSPACE_SLOTS {
        return Err(E_PERM);
    }
    let cs = cspaces();
    let cap = cs[proc_id as usize].slots[slot as usize];
    if cap.is_empty() {
        return Err(E_PERM);
    }
    if cap.kind != expected_kind {
        return Err(E_PERM);
    }
    if cap.rights & required_rights != required_rights {
        return Err(E_PERM);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// cap_mint
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_mint(parent_slot, target_slot, rights, badge) -> i32`.
///
/// Derive a child cap from the cap at `parent_slot` and install it
/// at `target_slot`, with the requested rights subset and badge.
///
/// Enforces INV-10 (rights monotonicity), INV-15 (reserved bits
/// rejected), INV-16 (kind/pool preservation), INV-18 (slot bounds).
pub fn cap_mint_impl(
    proc_id: u8,
    parent_slot: u32,
    target_slot: u32,
    rights: u32,
    badge: u32,
) -> i32 {
    // Bounds checks (INV-18).
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if parent_slot >= CSPACE_SLOTS as u32 || target_slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    if rights > 0xFF {
        // The full WASM i32 rights value must fit in a u8.
        return E_INVAL;
    }
    let parent_slot = parent_slot as u8;
    let target_slot = target_slot as u8;
    let rights = rights as u8;

    // Snapshot parent + parent_id while holding the cspaces borrow.
    let (parent_cap, parent_id) = {
        let cs = cspaces();
        let parent = cs[proc_id as usize].slots[parent_slot as usize];
        if parent.is_empty() {
            return E_INVAL;
        }
        if !cs[proc_id as usize].slots[target_slot as usize].is_empty() {
            return E_INVAL;
        }
        let gen = cs[proc_id as usize].generations[parent_slot as usize];
        let id = CapId::new(proc_id, parent_slot, gen);
        (parent, id)
    };

    // Pure-function derive (PR 1).
    let mut child = match Cap::derive(&parent_cap, parent_id, rights, badge) {
        Ok(c) => c,
        Err(crate::error::KernelError::PermissionDenied) => return E_PERM,
        Err(_) => return E_INVAL,
    };

    // The child carries the target slot's current generation so any
    // future revoke walk can detect ABA.
    let target_gen = {
        let cs = cspaces();
        cs[proc_id as usize].generations[target_slot as usize]
    };
    child.generation = target_gen as u32;

    // Install child + bump object refcount.
    {
        let cs = cspaces();
        cs[proc_id as usize].slots[target_slot as usize] = child;
    }
    inc_refcount(child.kind, child.pool_index);

    0
}

// ─────────────────────────────────────────────────────────────────
// cap_copy
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_copy(src_slot, target_slot) -> i32`.
///
/// Same-rights duplicate of the cap at `src_slot` into `target_slot`.
/// The new cap shares the parent of the source (sibling, not child).
/// Used by callers who want two slots referencing the same cap.
pub fn cap_copy_impl(proc_id: u8, src_slot: u32, target_slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if src_slot >= CSPACE_SLOTS as u32 || target_slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let src_slot = src_slot as u8;
    let target_slot = target_slot as u8;

    let copied = {
        let cs = cspaces();
        let src = cs[proc_id as usize].slots[src_slot as usize];
        if src.is_empty() {
            return E_INVAL;
        }
        if !cs[proc_id as usize].slots[target_slot as usize].is_empty() {
            return E_INVAL;
        }
        let target_gen = cs[proc_id as usize].generations[target_slot as usize];
        let mut c = src;
        // Copy is a sibling — same parent as src, same rights — but
        // gets the target slot's generation.
        c.generation = target_gen as u32;
        cs[proc_id as usize].slots[target_slot as usize] = c;
        c
    };
    inc_refcount(copied.kind, copied.pool_index);

    0
}

// ─────────────────────────────────────────────────────────────────
// cap_revoke
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_revoke(slot) -> i32`.
///
/// Revoke the cap at `slot` and every descendant. Requires the cap
/// to have `CAP_RIGHT_WRITE` (Phase 1b convention; PR 3 §10 Q6).
pub fn cap_revoke_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    // Permission check: caller must hold WRITE on the cap to revoke.
    {
        let cs = cspaces();
        let cap = cs[proc_id as usize].slots[slot as usize];
        if cap.is_empty() {
            return E_INVAL;
        }
        if cap.rights & CAP_RIGHT_WRITE == 0 {
            return E_PERM;
        }
    }

    match revoke(proc_id, slot) {
        Ok(()) => 0,
        Err(_) => E_INVAL,
    }
}

// ─────────────────────────────────────────────────────────────────
// cap_delete
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_delete(slot) -> i32`.
///
/// Remove the cap at `slot` without cascading. The kernel object's
/// refcount is decremented; the object is freed if the count hits
/// zero. Descendants of the deleted cap are NOT affected — they
/// become orphaned via INV-17 (their parent slot's generation will
/// no longer match) and get cleaned up on the next revoke walk that
/// touches them, or when their containing process exits.
pub fn cap_delete_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    let (kind, pool_index) = {
        let cs = cspaces();
        let slot_ref = &mut cs[proc_id as usize].slots[slot as usize];
        if slot_ref.is_empty() {
            return E_INVAL;
        }
        let info = (slot_ref.kind, slot_ref.pool_index);
        *slot_ref = Cap::empty();
        let g = &mut cs[proc_id as usize].generations[slot as usize];
        *g = g.saturating_add(1);
        info
    };
    dec_refcount(kind, pool_index);

    0
}

// ─────────────────────────────────────────────────────────────────
// nic_attach_queue — driver → kernel: bind a virtqueue's rings to
// the NIC. PR Net-4c.
// ─────────────────────────────────────────────────────────────────

use crate::validate::is_net_mmio_addr;

/// VirtIO MMIO transport register offsets used by the queue-attach
/// host fn. Must match the driver's view of the register set
/// (`drivers/net/src/lib.rs::VIRTIO_MMIO_*`).
const VIRTIO_MMIO_QUEUE_SEL:        u32 = 0x030;
const VIRTIO_MMIO_QUEUE_NUM_MAX:    u32 = 0x034;
const VIRTIO_MMIO_QUEUE_NUM:        u32 = 0x038;
const VIRTIO_MMIO_QUEUE_READY:      u32 = 0x044;
const VIRTIO_MMIO_QUEUE_DESC_LOW:   u32 = 0x080;
const VIRTIO_MMIO_QUEUE_DESC_HIGH:  u32 = 0x084;
const VIRTIO_MMIO_QUEUE_DRIVER_LOW: u32 = 0x090;
const VIRTIO_MMIO_QUEUE_DRIVER_HIGH:u32 = 0x094;
const VIRTIO_MMIO_QUEUE_DEVICE_LOW: u32 = 0x0a0;
const VIRTIO_MMIO_QUEUE_DEVICE_HIGH:u32 = 0x0a4;

/// QEMU virt VirtIO-net MMIO base (lockstep with `validate.rs` and
/// `drivers/net/src/lib.rs`). For Phase 1b VF2 builds, the host
/// fn returns `E_INVAL` — GMAC has a different setup pipeline.
#[cfg(feature = "qemu")]
const NIC_BASE_FOR_QUEUE_ATTACH: u32 = 0x1000_8000;

/// `wari::nic_attach_queue(queue_idx, desc_off, avail_off, used_off,
///                         queue_size) -> i32`.
///
/// PR Net-4c — the Tier-2 net driver calls this once per virtqueue
/// (rx then tx) after `FEATURES_OK` and before `DRIVER_OK`. The
/// kernel:
///
/// 1. Cap-checks `Net + WRITE` at slot 0 (the driver's root cap).
/// 2. Resolves the calling instance's WASM linear memory.
/// 3. Translates the three lin-mem offsets to physical addresses
///    (Phase-1b's bump-allocator arena is identity-mapped, so
///    kernel-virtual = physical for everything wasmi allocates).
/// 4. Bounds-checks every offset against the lin-mem size and the
///    queue-size requirements (descriptor 16-byte aligned, ring
///    2-byte aligned).
/// 5. Writes the VirtIO MMIO queue-config registers per VirtIO 1.2
///    §4.2.2.2: `QueueSel`, `QueueNum`, `QueueDescLow/High`,
///    `QueueDriverLow/High`, `QueueDeviceLow/High`, `QueueReady`.
///
/// Returns 0 on success, `E_PERM` on cap denial, `E_INVAL` on bad
/// arguments, `E_NOMEM` if the caller has no exported `memory`.
///
/// # Why this lives in the kernel and not the driver
///
/// The driver doesn't know its own physical address — that's a
/// kernel-side fact. Letting the driver compute PAs would require
/// exposing `lin_mem_base()` which leaks kernel memory layout to
/// signed user code. Doing the translation kernel-side keeps the
/// PA leak inside Tier 0 (the audit story stays clean).
pub fn nic_attach_queue_impl<T>(
    caller: &mut wasmi::Caller<'_, T>,
    proc_id: u8,
    queue_idx: u32,
    desc_off: u32,
    avail_off: u32,
    used_off: u32,
    queue_size: u32,
) -> i32 {
    // Cap check: driver holds Net + WRITE at slot 0.
    if (proc_id as usize) >= MAX_PROCS {
        return E_PERM;
    }
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[0]
    };
    if cap.is_empty()
        || !matches!(cap.kind, ObjectKind::Net)
        || cap.rights & CAP_RIGHT_WRITE == 0
    {
        return E_PERM;
    }

    // Argument validation.
    // - queue_idx: 0 = rx, 1 = tx for VirtIO-net
    // - queue_size: power of 2, ≤ 256 (VirtIO 1.2 §2.6 caps at 32768
    //   but Phase 1b uses much smaller; we cap conservatively)
    if queue_idx > 1 {
        return E_INVAL;
    }
    if queue_size == 0
        || queue_size > 256
        || !queue_size.is_power_of_two()
    {
        return E_INVAL;
    }
    // VirtIO 1.2 §2.6 alignment: descriptor table 16-byte, available
    // ring 2-byte, used ring 4-byte. Reject misaligned offsets.
    if desc_off & 0xF != 0 {
        return E_INVAL;
    }
    if avail_off & 0x1 != 0 {
        return E_INVAL;
    }
    if used_off & 0x3 != 0 {
        return E_INVAL;
    }

    // Resolve the caller's linear memory + compute physical addresses.
    let memory = match caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return E_NOMEM,
    };
    let mem_data = memory.data(&*caller);
    let mem_base = mem_data.as_ptr() as usize;
    let mem_len = mem_data.len();

    // Bounds check: every offset + the ring size it implies must fit
    // entirely within lin-mem.
    let desc_size = 16usize * queue_size as usize;
    let avail_size = 4usize + 2 * queue_size as usize;
    let used_size = 4usize + 8 * queue_size as usize;
    if (desc_off as usize)
        .checked_add(desc_size)
        .map_or(true, |end| end > mem_len)
    {
        return E_INVAL;
    }
    if (avail_off as usize)
        .checked_add(avail_size)
        .map_or(true, |end| end > mem_len)
    {
        return E_INVAL;
    }
    if (used_off as usize)
        .checked_add(used_size)
        .map_or(true, |end| end > mem_len)
    {
        return E_INVAL;
    }

    let desc_pa = mem_base + desc_off as usize;
    let avail_pa = mem_base + avail_off as usize;
    let used_pa = mem_base + used_off as usize;

    // Write VirtIO MMIO queue config. Phase-1b QEMU only — VF2
    // GMAC has a different ring layout, so this host fn returns
    // E_INVAL on vf2 builds.
    #[cfg(feature = "qemu")]
    {
        let base = NIC_BASE_FOR_QUEUE_ATTACH;

        // Defensive: validator-narrowed range. The kernel already
        // gates net_mmio_* host fns, but this path bypasses the
        // host fn (writes happen directly inside the kernel) — so
        // the validator narrows here too.
        if !is_net_mmio_addr((base + VIRTIO_MMIO_QUEUE_SEL) as usize) {
            return E_INVAL;
        }

        // Step 1 — select the queue we're configuring (§4.2.2.2).
        write32(base + VIRTIO_MMIO_QUEUE_SEL, queue_idx);

        // Step 2 — verify QueueNumMax ≥ our queue_size.
        let qmax = read32(base + VIRTIO_MMIO_QUEUE_NUM_MAX);
        if qmax < queue_size {
            return E_INVAL;
        }

        // Step 3 — set queue size.
        write32(base + VIRTIO_MMIO_QUEUE_NUM, queue_size);

        // Step 4 — write the three ring physical addresses.
        // Note: VirtIO 1.0+ uses 64-bit addresses split as Low/High.
        write32(base + VIRTIO_MMIO_QUEUE_DESC_LOW, desc_pa as u32);
        write32(base + VIRTIO_MMIO_QUEUE_DESC_HIGH, (desc_pa >> 32) as u32);
        write32(base + VIRTIO_MMIO_QUEUE_DRIVER_LOW, avail_pa as u32);
        write32(base + VIRTIO_MMIO_QUEUE_DRIVER_HIGH, (avail_pa >> 32) as u32);
        write32(base + VIRTIO_MMIO_QUEUE_DEVICE_LOW, used_pa as u32);
        write32(base + VIRTIO_MMIO_QUEUE_DEVICE_HIGH, (used_pa >> 32) as u32);

        // Step 5 — set QueueReady. Device starts using the queue.
        write32(base + VIRTIO_MMIO_QUEUE_READY, 1);

        0
    }
    #[cfg(feature = "vf2")]
    {
        // GMAC has a different ring setup pipeline (DMA descriptors,
        // not VirtIO virtqueues). Phase 1c will introduce the GMAC
        // equivalent host fn. Until then, the vf2 driver doesn't
        // call this and we hard-reject any caller that does.
        let _ = (queue_idx, desc_pa, avail_pa, used_pa, queue_size);
        E_INVAL
    }
}

/// Helper: 32-bit MMIO write to `addr` via `VolatilePtr`. Mirrors
/// the pattern in `runtime::host_fns::host_net_mmio_write32` but
/// without the extra cap check (the caller above already
/// cap-checked).
///
/// SAFETY: `addr` is a fixed VirtIO-net MMIO register inside the
/// `is_net_mmio_addr` window (validator-narrowed; INV-3 + INV-20).
fn write32(addr: u32, val: u32) {
    // SAFETY: INV-3 + INV-20. addr is from VirtIO MMIO base + a
    // spec-fixed register offset; the cap check above gates entry
    // to this helper.
    unsafe {
        core::ptr::write_volatile(addr as usize as *mut u32, val);
    }
}

fn read32(addr: u32) -> u32 {
    // SAFETY: same justification as `write32`.
    unsafe { core::ptr::read_volatile(addr as usize as *const u32) }
}

// ─────────────────────────────────────────────────────────────────
// nic_queue_notify — kick a virtqueue's QueueNotify register
// PR Net-4d.
// ─────────────────────────────────────────────────────────────────

const VIRTIO_MMIO_QUEUE_NOTIFY: u32 = 0x050;

/// `wari::nic_queue_notify(queue_idx) -> i32`.
///
/// PR Net-4d — the Tier-2 net driver calls this after writing
/// new entries into a virtqueue's available ring (or after
/// repopulating rx descriptors) to tell the device "queue
/// `queue_idx` has new buffers, look at it." VirtIO 1.2 §4.2.4.1.
///
/// Cap-gated by `Net + WRITE` at slot 0. Returns 0 on success,
/// `E_PERM` on cap denial, `E_INVAL` on bad queue_idx.
pub fn nic_queue_notify_impl(proc_id: u8, queue_idx: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_PERM;
    }
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[0]
    };
    if cap.is_empty()
        || !matches!(cap.kind, ObjectKind::Net)
        || cap.rights & CAP_RIGHT_WRITE == 0
    {
        return E_PERM;
    }
    if queue_idx > 1 {
        return E_INVAL;
    }
    #[cfg(feature = "qemu")]
    {
        write32(NIC_BASE_FOR_QUEUE_ATTACH + VIRTIO_MMIO_QUEUE_NOTIFY, queue_idx);
        0
    }
    #[cfg(feature = "vf2")]
    {
        let _ = queue_idx;
        E_INVAL
    }
}

// ─────────────────────────────────────────────────────────────────
// lin_mem_base — leak the driver's lin-mem PA so it can compute
// physical addresses for descriptor entries. PR Net-4d.
// ─────────────────────────────────────────────────────────────────

/// `wari::lin_mem_base() -> u64`.
///
/// Returns the physical address of the calling instance's WASM
/// linear-memory base. PR Net-4d adds this so the Tier-2 net driver
/// can compute physical addresses for VirtIO descriptor `addr`
/// fields (which are PAs, not lin-mem offsets).
///
/// **Why this leaks PA to user code (and why it's acceptable here)**:
/// the Tier-2 net driver is signed code with `Net + WRITE`
/// authority. Knowing its own lin-mem PA does not expand the kernel
/// memory it can reach (the WASM sandbox already restricts memory
/// access to its own lin-mem; PA knowledge doesn't change that).
/// The leak is bounded: only Net-cap holders learn the address, and
/// a Net cap mint is gated by INV-19 (Tier-1 cannot hold one). For
/// VirtIO descriptor setup specifically, the alternative (kernel
/// translates per-descriptor) would need a per-descriptor host fn
/// per packet path, which is much more host-fn surface and more
/// audit complexity.
///
/// Cap-gated by `Net + READ` at slot 0. Returns the PA on success;
/// 0 on cap denial (a real lin-mem base is never 0 since the bump
/// allocator's arena starts at the kernel's `_runtime_heap_start`,
/// well above 0x40200000 on QEMU virt).
pub fn lin_mem_base_impl<T>(caller: &mut wasmi::Caller<'_, T>, proc_id: u8) -> u64 {
    if (proc_id as usize) >= MAX_PROCS {
        return 0;
    }
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[0]
    };
    if cap.is_empty() || !matches!(cap.kind, ObjectKind::Net) {
        return 0;
    }
    if cap.rights & CAP_RIGHT_READ == 0 {
        return 0;
    }
    let memory = match caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return 0,
    };
    let data = memory.data(&*caller);
    data.as_ptr() as u64
}

// ─────────────────────────────────────────────────────────────────
// nic_set_mac — driver → kernel "I'm initialized, here's the MAC"
// ─────────────────────────────────────────────────────────────────

/// `wari::nic_set_mac(mac_low: u32, mac_high: u32) -> i32`.
///
/// PR Net-4b — the Tier-2 net driver calls this after it has
/// successfully completed the VirtIO device init sequence and read
/// the MAC from device config space. The kernel stores the MAC in
/// the driver's `Net` pool entry and flips `initialized = true` so
/// `runtime::run_tier2_net` can observe driver readiness.
///
/// Argument encoding: 6-byte MAC packed little-endian as
/// `mac_low = mac[0..4]` and `mac_high = mac[4..6]` (high 16 bits of
/// `mac_high` ignored). VirtIO 1.2 §5.1.4 says the MAC bytes are
/// "the device's MAC address. The mac field is only valid if
/// VIRTIO_NET_F_MAC has been negotiated" — driver is responsible
/// for that gate.
///
/// Cap-gated by `Net` cap with WRITE rights at slot 0 (the driver's
/// root cap from `init_root_caps`).
pub fn nic_set_mac_impl(proc_id: u8, mac_low: u32, mac_high: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_PERM;
    }
    // Cap check: driver holds Net + WRITE at slot 0.
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[0]
    };
    if cap.is_empty() {
        return E_PERM;
    }
    if !matches!(cap.kind, ObjectKind::Net) {
        return E_PERM;
    }
    if cap.rights & CAP_RIGHT_WRITE == 0 {
        return E_PERM;
    }

    // Update the Net pool entry.
    let pool_index = cap.pool_index;
    let pools = object_pools();
    if let Some(net) = pools.nets.get_mut(pool_index) {
        net.mac[0] = (mac_low & 0xFF) as u8;
        net.mac[1] = ((mac_low >> 8) & 0xFF) as u8;
        net.mac[2] = ((mac_low >> 16) & 0xFF) as u8;
        net.mac[3] = ((mac_low >> 24) & 0xFF) as u8;
        net.mac[4] = (mac_high & 0xFF) as u8;
        net.mac[5] = ((mac_high >> 8) & 0xFF) as u8;
        net.initialized = true;
        0
    } else {
        E_INVAL
    }
}

// ─────────────────────────────────────────────────────────────────
// notification_wait / notification_ack
// ─────────────────────────────────────────────────────────────────

/// `wari::notification_wait(slot) -> i32`.
///
/// Phase-1b **polling** primitive: returns `0` immediately if any
/// signal bit is set on the Notification at `slot`, `E_AGAIN` if
/// the bitmap is zero, `E_PERM` if the slot doesn't hold a
/// Notification cap with READ rights.
///
/// Drivers that need IRQ-driven processing call this in a loop
/// (yielding via `cap_lookup` or arbitrary host fns until the
/// kernel's trap dispatcher signals the bound IRQ).
///
/// Phase 2+ extends this to a real blocking primitive backed by a
/// scheduler wait queue; for Phase 1b polling is acceptable
/// because (a) the only caller is the net driver which is
/// re-entered by every Tier-1 socket call anyway, (b) we have no
/// preemption so a busy-wait blocks the system — by design,
/// drivers must check this once per dispatch and return.
pub fn notification_wait_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[slot as usize]
    };
    if cap.is_empty() {
        return E_PERM;
    }
    if !matches!(cap.kind, ObjectKind::Notification) {
        return E_PERM;
    }
    if cap.rights & CAP_RIGHT_READ == 0 {
        return E_PERM;
    }
    let pools = object_pools();
    if let Some(notif) = pools.notifications.get(cap.pool_index) {
        if notif.signals != 0 {
            0
        } else {
            E_AGAIN
        }
    } else {
        E_INVAL
    }
}

/// `wari::notification_ack(slot) -> i32`.
///
/// Clears all signal bits on the Notification at `slot`. Used by
/// drivers after they have processed the IRQ work and want to
/// re-arm for the next signal.
///
/// Phase 1b clears all bits at once (doesn't accept a per-bit
/// mask); the only caller is the single-IRQ-per-driver pattern
/// where there's nothing finer to ack.
pub fn notification_ack_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[slot as usize]
    };
    if cap.is_empty() {
        return E_PERM;
    }
    if !matches!(cap.kind, ObjectKind::Notification) {
        return E_PERM;
    }
    if cap.rights & CAP_RIGHT_READ == 0 {
        return E_PERM;
    }
    let pools = object_pools();
    if let Some(notif) = pools.notifications.get_mut(cap.pool_index) {
        notif.signals = 0;
        0
    } else {
        E_INVAL
    }
}

// ─────────────────────────────────────────────────────────────────
// cap_lookup
// ─────────────────────────────────────────────────────────────────

/// In-memory layout of `CapInfo` written by `cap_lookup`.
///
/// 8 bytes total, repr(C), little-endian (RISC-V default):
///
/// ```text
///   offset  size  field
///   ──────  ────  ────────
///   0       1     kind   (ObjectKind discriminant)
///   1       1     rights
///   2-3     2     _padding
///   4-7     4     badge
/// ```
///
/// Note: parent CapId, pool_index, and slot generation are
/// **not** exposed to userspace (kernel-internal — INV-15 and
/// design-doc §10 Q5).
const CAP_INFO_SIZE: usize = 8;

/// `wari::cap_lookup(slot, out_buf) -> i32`.
///
/// Read metadata for the cap at `slot` and write `CapInfo` (8 bytes)
/// to `out_buf` in the caller's WASM linear memory. Returns 0 on
/// success even if the slot is empty (the written `CapInfo` will
/// have `kind = Empty = 0`); errors are reserved for OOB slot,
/// missing memory export, and OOB linear-memory write.
pub fn cap_lookup_impl<T>(
    caller: &mut Caller<'_, T>,
    proc_id: u8,
    slot: u32,
    out_buf: u32,
) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    let (kind_disc, rights, badge) = {
        let cs = cspaces();
        let cap = cs[proc_id as usize].slots[slot as usize];
        (cap.kind as u8, cap.rights, cap.badge)
    };

    // Build the 8-byte CapInfo on the stack.
    let mut buf = [0u8; CAP_INFO_SIZE];
    buf[0] = kind_disc;
    buf[1] = rights;
    // bytes 2..4 are reserved padding, left zeroed
    buf[4..8].copy_from_slice(&badge.to_le_bytes());

    // Resolve the caller's linear memory and write the buffer.
    let memory = match caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return E_NOMEM,
    };
    if memory
        .write(&mut *caller, out_buf as usize, &buf)
        .is_err()
    {
        return E_NOMEM;
    }
    0
}

// ─────────────────────────────────────────────────────────────────
// net_socket_create / net_socket_close (PR Net-6b)
// ─────────────────────────────────────────────────────────────────
//
// Tier-1 socket API entry points. The Phase-2 driver-manifest
// contract guarantees the driver exports `socket_create` /
// `socket_close` with the right signatures (kind=Net manifest
// declares them); the kernel then validates the calling tier's
// Net cap, dispatches into the driver, allocates a Socket pool
// entry, and mints a Socket cap into the caller's CSpace.

/// `wari::net_socket_create(proto: u32, slot_for_cap: u32) -> i32`.
///
/// Allocates a smoltcp socket of `proto` (1=Tcp, 2=Udp) via the
/// Tier-2 net driver, then mints a Socket cap into the caller's
/// CSpace at `slot_for_cap`. Returns 0 on success, negative
/// errno on failure.
///
/// Errors:
/// - `E_INVAL` — bad proc_id / bad slot index / unknown proto
/// - `E_PERM`  — caller does not hold a Net cap with WRITE rights
///               at `crate::cap::boot::SLOT_NET`, OR target slot
///               is already occupied
/// - `E_NOMEM` — Socket pool exhausted, or driver returned errno
pub fn net_socket_create_impl(
    proc_id: u8,
    proto: u32,
    slot_for_cap: u32,
) -> i32 {
    use crate::cap::boot::SLOT_NET;

    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot_for_cap >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot_for_cap = slot_for_cap as u8;

    // 1. Validate Net cap at SLOT_NET, snapshot the parent ref.
    let (net_cap, parent_id) = {
        let cs = cspaces();
        let cap = cs[proc_id as usize].slots[SLOT_NET as usize];
        if cap.is_empty() || !matches!(cap.kind, ObjectKind::Net) {
            return E_PERM;
        }
        if cap.rights & CAP_RIGHT_WRITE == 0 {
            return E_PERM;
        }
        if !cs[proc_id as usize].slots[slot_for_cap as usize].is_empty() {
            return E_PERM;
        }
        let gen = cs[proc_id as usize].generations[SLOT_NET as usize];
        (cap, CapId::new(proc_id, SLOT_NET, gen))
    };

    // 2. Dispatch into driver — returns smoltcp socket handle as
    //    positive i32 or negative errno.
    // SAFETY: tier2_net::install ran during boot (run_tier2_net);
    // single-hart INV-1 + INV-8.
    let driver_ret = match unsafe { crate::runtime::tier2_net::socket_create(proto) } {
        Ok(v) => v,
        Err(_) => return E_NOMEM, // driver trapped
    };
    if driver_ret < 0 {
        return driver_ret; // driver-side errno (E_INVAL / E_NOMEM)
    }
    let smoltcp_handle = driver_ret as u32;

    // 3. Allocate a Socket pool entry that records parent Net pool
    //    index + driver's smoltcp handle for later close.
    let socket_pool_idx = {
        let pools = object_pools();
        match pools
            .sockets
            .alloc(super::objects::Socket::new(net_cap.pool_index, smoltcp_handle))
        {
            Ok(idx) => idx,
            Err(_) => {
                // Could not allocate — roll back the driver-side
                // socket so we don't leak smoltcp state.
                // SAFETY: same as create above.
                let _ = unsafe {
                    crate::runtime::tier2_net::socket_close(smoltcp_handle)
                };
                return E_NOMEM;
            }
        }
    };

    // 4. Mint Socket cap into caller's slot_for_cap. Refcount on
    //    the Socket pool entry is bumped by `inc_refcount` to track
    //    that this cap exists.
    let target_gen = {
        let cs = cspaces();
        cs[proc_id as usize].generations[slot_for_cap as usize] as u32
    };
    let cap = Cap {
        badge: 0,
        parent: parent_id,
        generation: target_gen,
        pool_index: socket_pool_idx,
        kind: ObjectKind::Socket,
        rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE,
    };
    {
        let cs = cspaces();
        cs[proc_id as usize].slots[slot_for_cap as usize] = cap;
    }
    inc_refcount(ObjectKind::Socket, socket_pool_idx);

    0
}

/// `wari::net_socket_close(slot: u32) -> i32`.
///
/// Tears down the Socket cap at `slot` — calls into the driver
/// to release the smoltcp socket, frees the Socket pool entry,
/// clears the cap, bumps the slot generation. Returns 0 on
/// success, negative errno on failure.
pub fn net_socket_close_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    // 1. Validate the cap, snapshot the smoltcp handle from the
    //    Socket pool entry.
    let (sock_cap, smoltcp_handle) = {
        let cs = cspaces();
        let cap = cs[proc_id as usize].slots[slot as usize];
        if cap.is_empty() || !matches!(cap.kind, ObjectKind::Socket) {
            return E_PERM;
        }
        let pools = object_pools();
        let sock = match pools.sockets.get(cap.pool_index) {
            Some(s) => s,
            None => return E_INVAL, // dangling cap
        };
        (cap, sock.smoltcp_handle)
    };

    // 2. Tell the driver to release the smoltcp socket. Failures
    //    here are surfaced but we proceed with cap cleanup so the
    //    Tier-1 cannot wedge on a stuck socket.
    // SAFETY: same as create above.
    let driver_ret = unsafe { crate::runtime::tier2_net::socket_close(smoltcp_handle) }
        .unwrap_or(-1);

    // 3. Drop the cap, dec refcount (free pool entry on 0).
    {
        let cs = cspaces();
        cs[proc_id as usize].slots[slot as usize] = Cap::empty();
        cs[proc_id as usize].generations[slot as usize] =
            cs[proc_id as usize].generations[slot as usize].wrapping_add(1);
    }
    dec_refcount(sock_cap.kind, sock_cap.pool_index);

    if driver_ret != 0 {
        // Surface the driver-side error to the caller.
        driver_ret
    } else {
        0
    }
}

/// `wari::net_socket_bind(slot, ip_be, port) -> i32` (PR Net-6c).
///
/// Caller must hold a Socket cap with WRITE rights at `slot`.
/// Dispatches into the driver's `socket_bind`. Returns 0 on
/// success, negative errno otherwise.
pub fn net_socket_bind_impl(
    proc_id: u8,
    slot: u32,
    ip_be: u32,
    port: u32,
) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let smoltcp_handle = match resolve_socket_handle(proc_id, slot as u8) {
        Ok(h) => h,
        Err(e) => return e,
    };
    // SAFETY: install ran during boot.
    match unsafe { crate::runtime::tier2_net::socket_bind(smoltcp_handle, ip_be, port) } {
        Ok(rc) => rc,
        Err(_) => E_NOMEM,
    }
}

/// `wari::net_socket_listen(slot, backlog) -> i32` (PR Net-6c).
///
/// Caller must hold a Socket cap with WRITE rights at `slot`.
/// Calls into the driver to mark the underlying smoltcp TCP
/// socket as listening on its previously-bound port.
pub fn net_socket_listen_impl(proc_id: u8, slot: u32, backlog: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let smoltcp_handle = match resolve_socket_handle(proc_id, slot as u8) {
        Ok(h) => h,
        Err(e) => return e,
    };
    // SAFETY: install ran during boot.
    match unsafe { crate::runtime::tier2_net::socket_listen(smoltcp_handle, backlog) } {
        Ok(rc) => rc,
        Err(_) => E_NOMEM,
    }
}

/// Helper shared by every Socket-cap-gated host fn: validate the
/// cap at `slot` is a Socket cap with WRITE rights, then read the
/// driver-side smoltcp handle from the Socket pool entry.
fn resolve_socket_handle(proc_id: u8, slot: u8) -> Result<u32, i32> {
    let cs = cspaces();
    let cap = cs[proc_id as usize].slots[slot as usize];
    if cap.is_empty() || !matches!(cap.kind, ObjectKind::Socket) {
        return Err(E_PERM);
    }
    if cap.rights & CAP_RIGHT_WRITE == 0 {
        return Err(E_PERM);
    }
    let pools = object_pools();
    let sock = pools.sockets.get(cap.pool_index).ok_or(E_INVAL)?;
    Ok(sock.smoltcp_handle)
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::types::{CAP_RIGHT_READ, CAP_RIGHT_WRITE};

    // These tests exercise the bounds-checking and rights paths.
    // Setup of populated CSpaces in tests requires touching the
    // global statics, which is awkward — full integration coverage
    // lives in the QEMU smoke test after PR 3a lands and in
    // `tests/security/cap_*.rs` (a follow-up PR).

    #[test]
    fn errno_values_distinct() {
        assert_ne!(E_PERM, E_INVAL);
        assert_ne!(E_PERM, E_NOMEM);
        assert_ne!(E_INVAL, E_NOMEM);
    }

    #[test]
    fn errno_values_are_negative() {
        assert!(E_PERM < 0);
        assert!(E_INVAL < 0);
        assert!(E_NOMEM < 0);
    }

    #[test]
    fn cap_mint_rejects_oob_proc_id() {
        let r = cap_mint_impl(MAX_PROCS as u8, 0, 1, CAP_RIGHT_READ as u32, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_mint_rejects_oob_parent_slot() {
        let r = cap_mint_impl(0, CSPACE_SLOTS as u32, 1, CAP_RIGHT_READ as u32, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_mint_rejects_oob_target_slot() {
        let r = cap_mint_impl(0, 0, CSPACE_SLOTS as u32, CAP_RIGHT_READ as u32, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_mint_rejects_oversize_rights() {
        let r = cap_mint_impl(0, 0, 1, 0xDEAD_BEEF, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_copy_rejects_oob_proc_id() {
        let r = cap_copy_impl(MAX_PROCS as u8, 0, 1);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_revoke_rejects_oob_slot() {
        let r = cap_revoke_impl(0, CSPACE_SLOTS as u32);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_delete_rejects_oob_proc_id() {
        let r = cap_delete_impl(MAX_PROCS as u8, 0);
        assert_eq!(r, E_INVAL);
    }
}
