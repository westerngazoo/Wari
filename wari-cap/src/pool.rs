// SPDX-License-Identifier: AGPL-3.0-only
//! Fixed-capacity slab pool + bounded queue.
//!
//! These are the two generic containers Phase-1b uses to back kernel
//! objects and IPC rendezvous state. Both are `no_std`-friendly,
//! allocation-free at runtime, and `const fn`-constructible so they
//! drop into static initializers without any boot-time work.
//!
//! ## `Pool<T, N>`
//!
//! A pool of up to `N` objects of type `T`. Used by `objects::ObjectPools`
//! to hold `Endpoint`, `Notification`, `Frame`, `Untyped`. `Cap`s
//! reference pool entries by `(kind, pool_index)`.
//!
//! Implementation: `slots: [Option<T>; N]`, with `alloc` performing a
//! linear scan for a `None` slot. Linear scan is `O(N)`; for our
//! pool sizes (16–1024) and Phase-1b workloads (single-digit
//! allocations per boot) this is dominated by everything else.
//! Phase 2+ may swap in an intrusive free-list if profiling shows
//! pool-alloc on a hot path.
//!
//! ## `BoundedQueue<T, N>`
//!
//! Fixed-size FIFO. Used by `Endpoint` for sender / receiver waiting
//! lists and by `Notification` for waiter tracking. Returns the
//! pushed value on overflow so the caller can react (typically with
//! `KernelError::OutOfHandles` after one retry).
//!
//! ## Why both live in one file
//!
//! Both are general containers with no cap-system semantics. Keeping
//! them in `cap::pool` rather than scattered into the consumers (per
//! the design doc's original sketch) trims four file boundaries and
//! makes the audit story trivial: one file owns all collection
//! invariants.

#![allow(clippy::doc_lazy_continuation)]

use wari_error::KernelError;

// ─────────────────────────────────────────────────────────────────
// Pool
// ─────────────────────────────────────────────────────────────────

/// Fixed-capacity slab pool. `alloc` returns an index into the
/// `slots` array; `dealloc` clears that index. Indices are stable
/// for the lifetime of the value (no compaction).
pub struct Pool<T, const N: usize> {
    slots: [Option<T>; N],
}

impl<T, const N: usize> Pool<T, N> {
    /// `const fn` constructor for static initialization. Every slot
    /// starts empty.
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; N],
        }
    }

    /// Insert a value into the first available slot, returning the
    /// slot index. Returns `KernelError::OutOfHandles` if the pool
    /// is full.
    pub fn alloc(&mut self, value: T) -> Result<u16, KernelError> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(value);
                return Ok(i as u16);
            }
        }
        Err(KernelError::OutOfHandles)
    }

    /// Remove and return the value at `index`. Returns
    /// `KernelError::InvalidArgument` if `index` is out of bounds or
    /// the slot is already empty.
    pub fn dealloc(&mut self, index: u16) -> Result<T, KernelError> {
        let i = index as usize;
        if i >= N {
            return Err(KernelError::InvalidArgument);
        }
        self.slots[i].take().ok_or(KernelError::InvalidArgument)
    }

    /// Read access to a pool slot.
    pub fn get(&self, index: u16) -> Option<&T> {
        let i = index as usize;
        if i < N {
            self.slots[i].as_ref()
        } else {
            None
        }
    }

    /// Mutable access to a pool slot.
    pub fn get_mut(&mut self, index: u16) -> Option<&mut T> {
        let i = index as usize;
        if i < N {
            self.slots[i].as_mut()
        } else {
            None
        }
    }

    /// `true` if the slot at `index` is currently allocated.
    pub fn is_allocated(&self, index: u16) -> bool {
        self.get(index).is_some()
    }

    /// Total capacity (compile-time constant, exposed for
    /// introspection in tests and diagnostics).
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Count of currently-allocated slots. `O(N)` linear scan.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// `true` if no slot is allocated.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T, const N: usize> Default for Pool<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// BoundedQueue
// ─────────────────────────────────────────────────────────────────

/// Fixed-size FIFO. `push` returns the value back on overflow so the
/// caller can decide how to handle (drop, retry, return error).
pub struct BoundedQueue<T, const N: usize> {
    items: [Option<T>; N],
    head: u16,
    tail: u16,
    len: u16,
}

impl<T, const N: usize> BoundedQueue<T, N> {
    /// `const fn` constructor.
    pub const fn new() -> Self {
        Self {
            items: [const { None }; N],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    /// Push to the tail. Returns `Err(value)` if the queue is full.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        if (self.len as usize) >= N {
            return Err(value);
        }
        self.items[self.tail as usize] = Some(value);
        self.tail = ((self.tail as usize + 1) % N) as u16;
        self.len += 1;
        Ok(())
    }

    /// Pop from the head. Returns `None` if the queue is empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let v = self.items[self.head as usize].take();
        self.head = ((self.head as usize + 1) % N) as u16;
        self.len -= 1;
        v
    }

    /// `true` if the queue holds zero items.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `true` if the queue cannot accept another `push`.
    pub const fn is_full(&self) -> bool {
        (self.len as usize) == N
    }

    /// Current item count.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Compile-time capacity.
    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<T, const N: usize> Default for BoundedQueue<T, N> {
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

    // ---- Pool ----

    #[test]
    fn fresh_pool_is_empty() {
        let p: Pool<u32, 4> = Pool::new();
        assert_eq!(p.len(), 0);
        assert!(p.is_empty());
        assert_eq!(p.capacity(), 4);
    }

    #[test]
    fn pool_alloc_returns_distinct_indices() {
        let mut p: Pool<u32, 4> = Pool::new();
        let a = p.alloc(10).unwrap();
        let b = p.alloc(20).unwrap();
        let c = p.alloc(30).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        assert_eq!(*p.get(a).unwrap(), 10);
        assert_eq!(*p.get(b).unwrap(), 20);
        assert_eq!(*p.get(c).unwrap(), 30);
    }

    #[test]
    fn pool_full_returns_error() {
        let mut p: Pool<u32, 2> = Pool::new();
        assert!(p.alloc(1).is_ok());
        assert!(p.alloc(2).is_ok());
        assert_eq!(p.alloc(3), Err(KernelError::OutOfHandles));
    }

    #[test]
    fn pool_dealloc_returns_value() {
        let mut p: Pool<u32, 4> = Pool::new();
        let i = p.alloc(42).unwrap();
        let v = p.dealloc(i).unwrap();
        assert_eq!(v, 42);
        assert!(!p.is_allocated(i));
    }

    #[test]
    fn pool_dealloc_empty_slot_errors() {
        let mut p: Pool<u32, 4> = Pool::new();
        assert_eq!(p.dealloc(0), Err(KernelError::InvalidArgument));
    }

    #[test]
    fn pool_dealloc_out_of_bounds_errors() {
        let mut p: Pool<u32, 4> = Pool::new();
        assert_eq!(p.dealloc(99), Err(KernelError::InvalidArgument));
    }

    #[test]
    fn pool_alloc_after_dealloc_reuses_slot() {
        let mut p: Pool<u32, 2> = Pool::new();
        let a = p.alloc(1).unwrap();
        let _ = p.alloc(2).unwrap();
        assert_eq!(p.alloc(3), Err(KernelError::OutOfHandles));
        let _ = p.dealloc(a).unwrap();
        let c = p.alloc(3).unwrap();
        assert_eq!(c, a); // first-fit reuse
    }

    #[test]
    fn pool_get_mut_allows_modification() {
        let mut p: Pool<u32, 4> = Pool::new();
        let i = p.alloc(7).unwrap();
        *p.get_mut(i).unwrap() = 99;
        assert_eq!(*p.get(i).unwrap(), 99);
    }

    #[test]
    fn pool_handles_non_copy_types() {
        // Smoke test: const-init array of `[const { None }; N]` should
        // work for non-Copy T (like a struct containing an array).
        struct Big {
            _data: [u8; 32],
        }
        let mut p: Pool<Big, 2> = Pool::new();
        let i = p.alloc(Big { _data: [0; 32] }).unwrap();
        assert!(p.is_allocated(i));
    }

    // ---- BoundedQueue ----

    #[test]
    fn fresh_queue_is_empty() {
        let q: BoundedQueue<u32, 4> = BoundedQueue::new();
        assert!(q.is_empty());
        assert!(!q.is_full());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn queue_push_pop_fifo() {
        let mut q: BoundedQueue<u32, 4> = BoundedQueue::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        q.push(3).unwrap();
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn queue_push_overflow_returns_value() {
        let mut q: BoundedQueue<u32, 2> = BoundedQueue::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        assert!(q.is_full());
        // Third push must fail and return the value back.
        assert_eq!(q.push(3), Err(3));
    }

    #[test]
    fn queue_wraps_around_buffer() {
        let mut q: BoundedQueue<u32, 3> = BoundedQueue::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        assert_eq!(q.pop(), Some(1));
        q.push(3).unwrap();
        q.push(4).unwrap();
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(4));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn queue_is_full_only_when_full() {
        let mut q: BoundedQueue<u32, 2> = BoundedQueue::new();
        assert!(!q.is_full());
        q.push(1).unwrap();
        assert!(!q.is_full());
        q.push(2).unwrap();
        assert!(q.is_full());
        let _ = q.pop();
        assert!(!q.is_full());
    }
}
