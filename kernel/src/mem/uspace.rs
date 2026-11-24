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

use super::kvm;
use crate::error::KernelError;
use wari_mem::page_alloc::PAGE_SIZE;
use wari_mem::page_table::{make_satp, va_parts, Pte, PteFlags, PT_ENTRIES};
use wari_mem::uspace::{check_user_page, is_kernel_slot, USER_WINDOW_SLOT};

/// One process's address space. Owns its root table, the user-window
/// tables below it and every page mapped there; [`destroy`] gives all
/// of them back. Not `Clone`: two owners would free twice.
///
/// [`destroy`]: AddressSpace::destroy
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
        for index in (0..PT_ENTRIES).filter(|&i| is_kernel_slot(i)) {
            let pte = kvm::read_pte(kernel_root, index);
            if pte.is_valid() {
                kvm::write_pte(root, index, pte);
            }
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
    /// allocated before a failure stay in the space and go with
    /// [`destroy`](Self::destroy).
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

    /// Free every page and table below the user window, then the root.
    /// If this space is active, the hart is moved to the kernel root
    /// first.
    ///
    /// Postcondition: no translation into this space survives in the
    /// TLB and the allocator has every page back. A page the allocator
    /// refuses is logged by `kvm::free_page` (R5: no panic).
    pub fn destroy(self) {
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

/// PA of the root the hart currently translates through.
fn current_root() -> usize {
    let satp: usize;
    // SAFETY: INV-7. `csrr satp` is a side-effect-free S-mode CSR read.
    unsafe { core::arch::asm!("csrr {0}, satp", out(reg) satp) };
    // Sv39 satp: PPN in bits 0..44.
    (satp & ((1 << 44) - 1)) * PAGE_SIZE
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
    // ahead of the first walk through the new root.
    unsafe {
        core::arch::asm!(
            "csrw satp, {0}",
            "sfence.vma zero, zero",
            in(reg) satp,
        );
    }
}
