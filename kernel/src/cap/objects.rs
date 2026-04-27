// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel objects referenced by capabilities.
//!
//! Phase 1b ships exactly four object kinds. Each is a plain
//! `#[repr(C)]` struct with a `const fn new()` constructor; nothing
//! here uses heap allocation, raw pointers, or `unsafe`.
//!
//! | Kind          | Purpose                                                |
//! |---------------|--------------------------------------------------------|
//! | `Endpoint`    | Synchronous IPC rendezvous point                       |
//! | `Notification`| Asynchronous binary signal (semaphore-like)            |
//! | `Frame`       | A 4 KiB physical page mappable into a VAS              |
//! | `Untyped`     | A pool of typed-as-untyped memory, retypable downstream|
//!
//! ## `TcbRef` placeholder
//!
//! Phase 1b's scheduler is hardcoded (see `runtime/`); there is no
//! real TCB cap kind yet. `TcbRef` is a `u8` newtype wrapping a
//! process id, used by `Endpoint` and `Notification` to track
//! waiters. Phase 2+ will replace `TcbRef` with `Cap<Tcb>` and the
//! callers shift accordingly.
//!
//! ## `ObjectPools`
//!
//! A single struct holding one `Pool<T, N>` per kind. The Phase-1b
//! capacities are deliberately fixed:
//!
//! | Pool                | Capacity | Memory cost |
//! |---------------------|----------|-------------|
//! | `endpoints`         |       64 |     ~1.5 KB |
//! | `notifications`     |       64 |     ~768 B  |
//! | `frames`            |     1024 |      ~16 KB |
//! | `untypeds`          |       16 |       ~256 B|
//!
//! These are constants in `cap::objects` rather than configurable.
//! "Configurable pool sizes" is a Phase 2+ ergonomic if Phase-1b
//! workloads ever pressure the limits — Simplicity First says we
//! don't pay the bug surface for flexibility we don't yet need.
//!
//! ## Why one file (deviation from the design doc's per-kind split)
//!
//! Each kernel object's struct definition is ~10–20 lines. The design
//! doc sketched five files (`objects/{mod,endpoint,notification,
//! untyped,frame}.rs`); collapsing them into this single file trims
//! four module boundaries and lets the audit reader see all four
//! kinds on one screen. The cost is a slightly longer file (~250
//! LOC); the benefit is clearer cross-kind comparison and a single
//! `pub use` surface in `cap::mod`. If a kind ever grows past ~50
//! LOC of state-machine logic, it gets its own file.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use super::pool::{BoundedQueue, Pool};

// ─────────────────────────────────────────────────────────────────
// TcbRef — Phase 1b scheduler placeholder
// ─────────────────────────────────────────────────────────────────

/// Reference to a thread/process the scheduler is tracking. In
/// Phase 1b this is just the process id (the kernel's hardcoded
/// scheduler resolves it directly). Phase 2+ replaces this with a
/// `Cap` of `ObjectKind::Tcb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcbRef(pub u8);

// ─────────────────────────────────────────────────────────────────
// Endpoint — synchronous IPC
// ─────────────────────────────────────────────────────────────────

/// Synchronous IPC rendezvous point.
///
/// Senders and receivers queue up; when one side has a peer waiting,
/// the IPC happens (PR 3 implements the actual transfer). Until then,
/// the queues just track who is waiting.
///
/// Refcount is bumped on every cap minted to this endpoint and
/// decremented on revoke/delete. When refcount hits zero the
/// endpoint is returned to its pool.
#[repr(C)]
pub struct Endpoint {
    /// Queue of senders waiting to deliver a message.
    pub senders: BoundedQueue<TcbRef, 8>,
    /// Queue of receivers waiting for a message.
    pub receivers: BoundedQueue<TcbRef, 8>,
    /// Number of caps currently pointing to this endpoint.
    pub refcount: u16,
}

impl Endpoint {
    pub const fn new() -> Self {
        Self {
            senders: BoundedQueue::new(),
            receivers: BoundedQueue::new(),
            refcount: 0,
        }
    }
}

impl Default for Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// Notification — asynchronous signal
// ─────────────────────────────────────────────────────────────────

/// Asynchronous binary signal. Used for IRQ-style wakeups and other
/// non-blocking notifications.
///
/// `signals` is a 32-bit bitmap; senders set bits, the receiver
/// (after `wait`) reads and clears them. `waiter` is set when a
/// process is blocked waiting for any pending signal; the next
/// signal-set delivery wakes it.
#[repr(C)]
pub struct Notification {
    /// Bitmap of pending signals. One bit per badge.
    pub signals: u32,
    /// Receiver currently waiting, if any.
    pub waiter: Option<TcbRef>,
    /// Number of caps currently pointing to this notification.
    pub refcount: u16,
}

impl Notification {
    pub const fn new() -> Self {
        Self {
            signals: 0,
            waiter: None,
            refcount: 0,
        }
    }
}

impl Default for Notification {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// Frame — a mappable physical page
// ─────────────────────────────────────────────────────────────────

/// A 4 KiB physical page that can be mapped into a virtual address
/// space.
///
/// `pa` is the physical address of the page (4 KiB-aligned). Phase
/// 1b doesn't yet ship a typed `PhysAddr` wrapper — the `usize` here
/// is the same shape the existing `wari-mem` crate uses internally.
/// A `PhysAddr` newtype lands when the cap-mediated page-mapping
/// syscall arrives (PR 3 or Phase 2+).
#[repr(C)]
pub struct Frame {
    /// Physical address of the 4 KiB page (must be 0x1000-aligned).
    pub pa: usize,
    /// Number of caps currently pointing to this frame.
    pub refcount: u16,
}

impl Frame {
    pub const fn new(pa: usize) -> Self {
        Self { pa, refcount: 0 }
    }
}

// ─────────────────────────────────────────────────────────────────
// Untyped — typed-as-untyped memory pool
// ─────────────────────────────────────────────────────────────────

/// A pool of memory typed as `Untyped`. Retyping converts a chunk
/// into another kernel object kind (Endpoint, Notification, Frame,
/// or smaller Untyped sub-pool — Phase 2+ only).
///
/// Phase 1b ships untypeds for boot-time use only; userspace cannot
/// retype yet. The `watermark` field tracks how much of the pool
/// has been retyped already.
#[repr(C)]
pub struct Untyped {
    /// Physical address of the start of the pool.
    pub pa: usize,
    /// Pool size in bits (12 = 4 KiB, 13 = 8 KiB, …, max 22 = 4 MiB).
    pub size_bits: u8,
    /// Bytes already retyped out of this pool.
    pub watermark: usize,
    /// Number of caps currently pointing to this untyped.
    pub refcount: u16,
}

impl Untyped {
    pub const fn new(pa: usize, size_bits: u8) -> Self {
        Self {
            pa,
            size_bits,
            watermark: 0,
            refcount: 0,
        }
    }

    /// Total pool size in bytes.
    pub const fn size_bytes(&self) -> usize {
        1usize << self.size_bits
    }

    /// Bytes still available for retype.
    pub const fn bytes_remaining(&self) -> usize {
        self.size_bytes().saturating_sub(self.watermark)
    }
}

// ─────────────────────────────────────────────────────────────────
// Pool capacities
// ─────────────────────────────────────────────────────────────────

/// Maximum number of `Endpoint` objects in the global pool.
pub const ENDPOINT_POOL_CAPACITY: usize = 64;
/// Maximum number of `Notification` objects in the global pool.
pub const NOTIFICATION_POOL_CAPACITY: usize = 64;
/// Maximum number of `Frame` objects in the global pool. 1024 frames
/// × 4 KiB = 4 MiB total tracked frame memory in Phase 1b.
pub const FRAME_POOL_CAPACITY: usize = 1024;
/// Maximum number of `Untyped` objects in the global pool.
pub const UNTYPED_POOL_CAPACITY: usize = 16;
/// Maximum number of `Net` objects (NIC handles). Sized for two
/// NICs × generation headroom; per net-driver-design.md §6.2.
pub const NET_POOL_CAPACITY: usize = 4;
/// Maximum number of `Socket` objects. Sized for a meaningful
/// concurrent-connection demo; per net-driver-design.md §6.3.
pub const SOCKET_POOL_CAPACITY: usize = 256;

// ─────────────────────────────────────────────────────────────────
// Net — NIC handle (driver-only)
// ─────────────────────────────────────────────────────────────────

/// NIC handle. Held by the Tier-2 net driver as a root cap;
/// Tier-1 tenants cannot hold this kind (INV-19).
///
/// `nic_kind`: 0 = VirtIO-net (QEMU), 1 = JH7110 GMAC eth0,
/// 2 = JH7110 GMAC eth1.
///
/// `mac` is populated by the driver via `wari::nic_set_mac` after
/// it reads the MAC from VirtIO config space (PR Net-4b). Until
/// `initialized = true`, the MAC field is zeroes.
#[repr(C)]
pub struct Net {
    /// Hardware target (see above).
    pub nic_kind: u8,
    /// Whether the driver has finished bringing the NIC up.
    pub initialized: bool,
    /// MAC address read from device config space at NIC bring-up.
    pub mac: [u8; 6],
    /// Number of `Socket` objects currently associated with this NIC.
    pub socket_count: u16,
    /// Cap refcount.
    pub refcount: u16,
}

impl Net {
    pub const fn new(nic_kind: u8) -> Self {
        Self {
            nic_kind,
            initialized: false,
            mac: [0; 6],
            socket_count: 0,
            refcount: 0,
        }
    }
}

impl Default for Net {
    fn default() -> Self {
        Self::new(0)
    }
}

// ─────────────────────────────────────────────────────────────────
// Socket — per-tenant TCP/UDP socket
// ─────────────────────────────────────────────────────────────────

/// Per-tenant TCP/UDP socket. Minted from a `Net` cap by the
/// driver in response to `wari::net_socket_create`; granted to the
/// calling Tier-1 tenant via cap-IPC.
#[repr(C)]
pub struct Socket {
    /// Pool index of the parent `Net` this socket lives on.
    pub net_idx: u16,
    /// Opaque smoltcp socket handle (kernel does not interpret).
    pub smoltcp_handle: u32,
    /// Local 4-tuple (big-endian IPv4; 0 fields = unbound).
    pub local_ip: u32,
    pub local_port: u16,
    pub peer_ip: u32,
    pub peer_port: u16,
    /// Cap refcount.
    pub refcount: u16,
}

impl Socket {
    pub const fn new(net_idx: u16, smoltcp_handle: u32) -> Self {
        Self {
            net_idx,
            smoltcp_handle,
            local_ip: 0,
            local_port: 0,
            peer_ip: 0,
            peer_port: 0,
            refcount: 0,
        }
    }
}

impl Default for Socket {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

// ─────────────────────────────────────────────────────────────────
// ObjectPools
// ─────────────────────────────────────────────────────────────────

/// Global pools of all kernel object kinds, one per kind.
///
/// Lives as a single static in `cap::storage`; mutated only through
/// the cap subsystem (boot-time root mints, future syscalls).
pub struct ObjectPools {
    pub endpoints: Pool<Endpoint, ENDPOINT_POOL_CAPACITY>,
    pub notifications: Pool<Notification, NOTIFICATION_POOL_CAPACITY>,
    pub frames: Pool<Frame, FRAME_POOL_CAPACITY>,
    pub untypeds: Pool<Untyped, UNTYPED_POOL_CAPACITY>,
    pub nets: Pool<Net, NET_POOL_CAPACITY>,
    pub sockets: Pool<Socket, SOCKET_POOL_CAPACITY>,
}

impl ObjectPools {
    pub const fn new() -> Self {
        Self {
            endpoints: Pool::new(),
            notifications: Pool::new(),
            frames: Pool::new(),
            untypeds: Pool::new(),
            nets: Pool::new(),
            sockets: Pool::new(),
        }
    }
}

impl Default for ObjectPools {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Endpoint ----

    #[test]
    fn endpoint_starts_empty() {
        let ep = Endpoint::new();
        assert!(ep.senders.is_empty());
        assert!(ep.receivers.is_empty());
        assert_eq!(ep.refcount, 0);
    }

    #[test]
    fn endpoint_queues_have_correct_capacity() {
        let ep = Endpoint::new();
        assert_eq!(ep.senders.capacity(), 8);
        assert_eq!(ep.receivers.capacity(), 8);
    }

    // ---- Notification ----

    #[test]
    fn notification_starts_empty() {
        let n = Notification::new();
        assert_eq!(n.signals, 0);
        assert!(n.waiter.is_none());
        assert_eq!(n.refcount, 0);
    }

    // ---- Frame ----

    #[test]
    fn frame_records_pa() {
        let f = Frame::new(0x4020_0000);
        assert_eq!(f.pa, 0x4020_0000);
        assert_eq!(f.refcount, 0);
    }

    // ---- Untyped ----

    #[test]
    fn untyped_size_bits_to_bytes() {
        let u = Untyped::new(0x5000_0000, 12);
        assert_eq!(u.size_bytes(), 4096);
        let u = Untyped::new(0x5000_0000, 22);
        assert_eq!(u.size_bytes(), 1 << 22);
    }

    #[test]
    fn untyped_remaining_starts_at_size() {
        let u = Untyped::new(0, 12);
        assert_eq!(u.bytes_remaining(), 4096);
    }

    #[test]
    fn untyped_remaining_after_watermark() {
        let mut u = Untyped::new(0, 13); // 8 KiB
        u.watermark = 3000;
        assert_eq!(u.bytes_remaining(), 8192 - 3000);
    }

    #[test]
    fn untyped_remaining_saturates_at_zero() {
        let mut u = Untyped::new(0, 12);
        u.watermark = 99999; // way past the 4 KiB
        assert_eq!(u.bytes_remaining(), 0);
    }

    // ---- ObjectPools ----

    #[test]
    fn object_pools_start_empty() {
        let pools = ObjectPools::new();
        assert_eq!(pools.endpoints.len(), 0);
        assert_eq!(pools.notifications.len(), 0);
        assert_eq!(pools.frames.len(), 0);
        assert_eq!(pools.untypeds.len(), 0);
    }

    #[test]
    fn object_pools_capacities_match_constants() {
        let pools = ObjectPools::new();
        assert_eq!(pools.endpoints.capacity(), ENDPOINT_POOL_CAPACITY);
        assert_eq!(pools.notifications.capacity(), NOTIFICATION_POOL_CAPACITY);
        assert_eq!(pools.frames.capacity(), FRAME_POOL_CAPACITY);
        assert_eq!(pools.untypeds.capacity(), UNTYPED_POOL_CAPACITY);
    }

    #[test]
    fn object_pools_alloc_endpoint() {
        let mut pools = ObjectPools::new();
        let i = pools.endpoints.alloc(Endpoint::new()).unwrap();
        assert!(pools.endpoints.is_allocated(i));
        assert_eq!(pools.endpoints.get(i).unwrap().refcount, 0);
    }
}
