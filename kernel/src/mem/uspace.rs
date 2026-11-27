// SPDX-License-Identifier: AGPL-3.0-only
//! Per-process Sv39 address spaces (ADR-004 step 2) — unsafe glue over
//! `wari_mem::uspace`'s layout and `kvm`'s table helpers.
//!
//! A space is a root table whose kernel slots are copies of the kernel
//! root's (same lower tables, U=0, GLOBAL) and whose user-window slot
//! holds only pages this space allocated. Those pages are never mapped
//! anywhere else with U=1 (INV-28), and the kernel reaches them only
//! through their identity-mapped PA, never through the U=1 alias
//! (`sstatus.SUM` stays 0).
//!
//! Every space runs with ASID 0 and every switch flushes the whole TLB
//! (`sfence.vma zero, zero`). ASIDs wait for the step-2 measurement of
//! the JH7110's real ASID width; until then a full flush is the only
//! choice that cannot leave one process's translation visible to
//! another.
//!
//! The one place the kernel reads a process's memory is its IPC buffer
//! (ADR-004 step 3, INV-31): a fixed page, found by walking the active
//! root, read through its identity PA for at most one page.

use super::kvm;
use crate::error::KernelError;
use wari_abi::umode::{IPC_BUFFER_LEN, IPC_BUFFER_VA};
use wari_mem::page_alloc::PAGE_SIZE;
use wari_mem::page_table::{make_satp, va_parts, walk, Pte, PteFlags, PT_ENTRIES};
use wari_mem::uspace::{
    check_user_page, ipc_buffer_page, is_kernel_slot, USER_WINDOW_BASE, USER_WINDOW_END,
    USER_WINDOW_SLOT,
};

// The ABI's IPC-buffer address is a page inside the user window: every
// space can map it, and reading it never reaches a kernel slot.
const _: () = assert!(IPC_BUFFER_LEN == PAGE_SIZE);
const _: () = assert!(IPC_BUFFER_VA.is_multiple_of(PAGE_SIZE));
const _: () = assert!(IPC_BUFFER_VA >= USER_WINDOW_BASE);
const _: () = assert!(IPC_BUFFER_VA + IPC_BUFFER_LEN <= USER_WINDOW_END);

/// Mask of the PPN field of an Sv39 `satp` (bits 0..44).
const SATP_PPN_MASK: usize = (1 << 44) - 1;

/// One process's address space. Owns its root table, the user-window
/// tables below it and every page mapped there; dropping it gives all
/// of them back (RAII, like a `Box`), so no error path can leak one.
/// Not `Clone`: two owners would free twice.
#[must_use]
pub struct AddressSpace {
    root: usize,
}

impl AddressSpace {
    /// Allocate a root table and copy the kernel's root entries into it.
    ///
    /// Precondition: `kvm::init` has run and the kernel root is frozen
    /// (no root-level entry is added after boot; kernel mappings made
    /// later would not reach existing spaces).
    /// Errors: `InvalidArgument` if the MMU is not up or the kernel
    /// root uses the user-window slot; `OutOfPages` if the allocator is
    /// empty.
    pub fn create() -> Result<Self, KernelError> {
        let kernel_root = kvm::kernel_root().ok_or(KernelError::InvalidArgument)?;
        if kvm::read_pte(kernel_root, USER_WINDOW_SLOT).is_valid() {
            return Err(KernelError::InvalidArgument);
        }
        let root = kvm::alloc_zeroed_page()?;
        // `root` is zeroed, so copying the kernel's empty entries too is
        // harmless and keeps this a plain copy.
        for index in (0..PT_ENTRIES).filter(|&i| is_kernel_slot(i)) {
            kvm::write_pte(root, index, kvm::read_pte(kernel_root, index));
        }
        Ok(Self { root })
    }

    /// Map a fresh zeroed page at `va` with `flags` and return its
    /// physical address, through which the kernel may fill it.
    ///
    /// Errors: `InvalidArgument` if `va`/`flags` fail
    /// `wari_mem::uspace::check_user_page` or `va` is already mapped
    /// (no remapping: a page is owned by one mapping, INV-28);
    /// `OutOfPages` if the allocator is empty. Intermediate tables
    /// allocated before a failure stay in the space and go when it is
    /// dropped.
    pub fn map_zeroed(&mut self, va: usize, flags: PteFlags) -> Result<usize, KernelError> {
        check_user_page(va, flags).map_err(|_| KernelError::InvalidArgument)?;
        let (vpn2, vpn1, vpn0, _) = va_parts(va);
        let l1 = kvm::walk_or_create(self.root, vpn2)?;
        let l0 = kvm::walk_or_create(l1, vpn1)?;
        if kvm::read_pte(l0, vpn0).is_valid() {
            return Err(KernelError::InvalidArgument);
        }
        let page = kvm::alloc_zeroed_page()?;
        kvm::write_pte(l0, vpn0, Pte::new(page, flags));
        Ok(page)
    }

    /// Make this the space the hart translates through.
    ///
    /// The kernel is mapped identically in every space, so the code
    /// running this keeps executing; only the user window changes.
    pub fn activate(&self) {
        switch_to(self.root);
    }
}

impl Drop for AddressSpace {
    /// Free every page and table below the user window, then the root.
    /// If this space is active, the hart is moved to the kernel root
    /// first.
    ///
    /// Postcondition: the allocator has every page back. A page the
    /// allocator refuses is logged by `kvm::free_page` (R5: no panic).
    ///
    /// TLB: no translation into this space survives, but only because
    /// every space shares ASID 0 and every `switch_to` flushes the
    /// whole TLB — leaving the space (now or earlier) already dropped
    /// its entries, and the walker cannot reach a root not in `satp`.
    /// Once spaces get their own ASIDs (step 2's second PR) a switch
    /// no longer flushes everything, and this must issue
    /// `sfence.vma zero, <asid>` before freeing any page.
    fn drop(&mut self) {
        if current_root() == self.root {
            activate_kernel();
        }
        let window = kvm::read_pte(self.root, USER_WINDOW_SLOT);
        if window.is_branch() {
            let l1 = window.phys_addr();
            for i1 in 0..PT_ENTRIES {
                let mid = kvm::read_pte(l1, i1);
                if !mid.is_branch() {
                    continue;
                }
                let l0 = mid.phys_addr();
                for i0 in 0..PT_ENTRIES {
                    let leaf = kvm::read_pte(l0, i0);
                    if leaf.is_leaf() {
                        kvm::free_page(leaf.phys_addr());
                    }
                }
                kvm::free_page(l0);
            }
            kvm::free_page(l1);
        }
        kvm::free_page(self.root);
    }
}

/// Translate through the kernel root again (the space of the kernel's
/// own idle and boot code). A no-op before `kvm::init`.
pub fn activate_kernel() {
    if let Some(root) = kvm::kernel_root() {
        switch_to(root);
    }
}

/// Hand `f` the first `len` bytes of the IPC buffer of the space the
/// hart translates through now, and return what `f` returns.
///
/// Precondition: called from the trap of the process whose space is
/// active (its `satp` is still live while the kernel handles the trap);
/// `len <= IPC_BUFFER_LEN`, as `wari_sched::utrap::decide` guarantees.
/// Returns `None` if `len` is larger, or the active space has no IPC
/// buffer at `IPC_BUFFER_VA` (`wari_mem::uspace::ipc_buffer_page`): the
/// caller answers the syscall with an error.
pub fn with_current_ipc_buffer<R>(len: usize, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    if len > IPC_BUFFER_LEN {
        return None;
    }
    let found = walk(current_root(), IPC_BUFFER_VA, |pte_pa| {
        let table = pte_pa & !(PAGE_SIZE - 1);
        kvm::read_pte(table, (pte_pa - table) / core::mem::size_of::<u64>()).bits()
    });
    let page = ipc_buffer_page(found)?;
    // SAFETY: INV-5, INV-28, INV-31. `page` is the leaf the active
    // space maps at `IPC_BUFFER_VA` as a user data page; every such page
    // is a fresh allocator page in the identity-mapped heap, owned by
    // that one space (`map_zeroed`), and read here through its U=0
    // identity alias (`sstatus.SUM` stays 0). `len <= PAGE_SIZE`, so the
    // slice stays in the page. The process is stopped in its trap
    // (INV-2: single hart, interrupts masked), so nothing writes the
    // page while `f` holds the slice, and the slice does not outlive
    // this call.
    let bytes = unsafe { core::slice::from_raw_parts(page as *const u8, len) };
    Some(f(bytes))
}

/// PA of the root the hart currently translates through.
fn current_root() -> usize {
    let satp: usize;
    // SAFETY: INV-7. `csrr satp` is a side-effect-free S-mode CSR read.
    unsafe { core::arch::asm!("csrr {0}, satp", out(reg) satp) };
    (satp & SATP_PPN_MASK) * PAGE_SIZE
}

fn switch_to(root: usize) {
    let satp = make_satp(root, 0);
    // SAFETY: INV-7, INV-28. Every root passed here is the kernel root
    // or one built by `AddressSpace::create`, which maps the kernel
    // exactly as the kernel root does, so the next fetch and every
    // kernel access still resolve.
    //
    // Memory ordering (R6): the `sfence.vma zero, zero` after the write
    // flushes every translation of every ASID, so nothing translated
    // through the previous root (all spaces share ASID 0) is used
    // after the switch, and orders the PTE stores made before the call
    // ahead of the first walk through the new root. No fence is needed
    // before the write: the only access between the two instructions
    // is the fetch of the `sfence` itself, in kernel text, which is
    // GLOBAL and identical in every root — nothing in the user window.
    unsafe {
        core::arch::asm!(
            "csrw satp, {0}",
            "sfence.vma zero, zero",
            in(reg) satp,
        );
    }
}
