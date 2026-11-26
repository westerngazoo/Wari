// SPDX-License-Identifier: AGPL-3.0-only
//! Layout of a process address space (ADR-004 step 2): where a
//! process's own pages may live and which root-table entries belong to
//! the kernel.
//!
//! Every process gets its own Sv39 root table. The kernel's half is
//! *shared*, not rebuilt: the process root holds copies of the kernel
//! root's entries, which point at the same lower-level tables, all U=0
//! and GLOBAL. A process's own pages live in exactly one root slot, the
//! **user window** — a 1 GiB region the kernel's identity map never
//! touches. Destroying a space frees everything below that one slot and
//! nothing else.
//!
//! Prior art: copying the kernel's top-level entries into every new
//! root is seL4's `copyGlobalMappings` on RISC-V and Linux's
//! `pgd_alloc` from `swapper_pg_dir`. Like seL4 (and unlike Linux, which
//! opens `sstatus.SUM` in `copy_from_user`), Wari never lets the kernel
//! dereference a U=1 mapping: it reaches a process's pages through
//! their identity-mapped physical address, which is U=0.
//!
//! Pure: no `unsafe`, no MMIO, host-testable.

use crate::page_alloc::PAGE_SIZE;
use crate::page_table::{va_parts, PteFlags, PT_ENTRIES};

/// Root-table index (VPN[2]) of the user window.
///
/// The kernel identity-maps device MMIO below 1 GiB (slot 0) and RAM
/// from 0x4000_0000 (VF2) or 0x8000_0000 (QEMU) upwards, so it uses the
/// lowest handful of slots. Slot 128 stays clear of 128 GiB of RAM and
/// is in the lower half of the Sv39 space (slots 0..256 are the
/// positive, non-sign-extended VAs).
pub const USER_WINDOW_SLOT: usize = 128;

// The window must be in the lower half of Sv39 (slots 0..256): those
// VAs need no sign extension.
const _: () = assert!(USER_WINDOW_SLOT < PT_ENTRIES / 2);

/// Size of the root-level region one slot covers: 1 GiB.
pub const SLOT_SPAN: usize = 1 << 30;

/// First VA of the user window: `0x20_0000_0000`.
pub const USER_WINDOW_BASE: usize = USER_WINDOW_SLOT * SLOT_SPAN;

/// One past the last VA of the user window.
pub const USER_WINDOW_END: usize = USER_WINDOW_BASE + SLOT_SPAN;

/// Why a user mapping request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UspaceError {
    /// The VA is not 4 KiB aligned.
    Unaligned,
    /// The VA is outside the user window.
    OutsideWindow,
    /// The flags are not a valid user leaf: VALID, USER and at least
    /// one of R/W/X; never GLOBAL (a global leaf would survive an ASID
    /// switch into another process); never W without R (reserved in
    /// Sv39); never W together with X (W^X: a process cannot write code
    /// it then runs, stricter than Linux, which allows RWX mappings).
    BadFlags,
}

/// Check that `va` may hold a process page with `flags`.
///
/// Postcondition on `Ok`: `va` is page aligned, inside
/// `[USER_WINDOW_BASE, USER_WINDOW_END)`, and `flags` describe a
/// non-global, non-W+X user leaf.
///
/// ```
/// use wari_mem::uspace::{check_user_page, UspaceError, USER_WINDOW_BASE};
/// use wari_mem::page_table::{USER_RW, KERNEL_RW};
/// assert_eq!(check_user_page(USER_WINDOW_BASE, USER_RW), Ok(()));
/// assert_eq!(check_user_page(USER_WINDOW_BASE, KERNEL_RW), Err(UspaceError::BadFlags));
/// ```
pub fn check_user_page(va: usize, flags: PteFlags) -> Result<(), UspaceError> {
    if !va.is_multiple_of(PAGE_SIZE) {
        return Err(UspaceError::Unaligned);
    }
    if !(USER_WINDOW_BASE..USER_WINDOW_END).contains(&va) {
        return Err(UspaceError::OutsideWindow);
    }
    let user_leaf = flags.contains(PteFlags::VALID)
        && flags.contains(PteFlags::USER)
        && flags.is_leaf()
        && !flags.contains(PteFlags::GLOBAL);
    // A writable page must be readable (W without R is reserved) and
    // must not be executable (W^X).
    let write_ok = !flags.contains(PteFlags::WRITE)
        || (flags.contains(PteFlags::READ) && !flags.contains(PteFlags::EXECUTE));
    let leaf = user_leaf && write_ok;
    if !leaf {
        return Err(UspaceError::BadFlags);
    }
    Ok(())
}

/// Is root-table entry `index` one a process root copies from the
/// kernel root? Every slot except the user window.
///
/// Precondition: `index < PT_ENTRIES`; larger values return `false`.
pub fn is_kernel_slot(index: usize) -> bool {
    index < PT_ENTRIES && index != USER_WINDOW_SLOT
}

/// The root slot `va` falls in, as `va_parts` computes it.
pub fn root_slot(va: usize) -> usize {
    va_parts(va).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_table::{KERNEL_RW, USER_RW, USER_RX};

    #[test]
    fn window_is_exactly_one_root_slot() {
        assert_eq!(USER_WINDOW_BASE, 0x20_0000_0000);
        assert_eq!(root_slot(USER_WINDOW_BASE), USER_WINDOW_SLOT);
        assert_eq!(root_slot(USER_WINDOW_END - PAGE_SIZE), USER_WINDOW_SLOT);
        assert_ne!(root_slot(USER_WINDOW_END), USER_WINDOW_SLOT);
        assert_ne!(root_slot(USER_WINDOW_BASE - PAGE_SIZE), USER_WINDOW_SLOT);
    }

    #[test]
    fn window_does_not_overlap_the_kernel_identity_map() {
        // RAM bases of both boards and the device MMIO below them all
        // land in slots well below the window.
        for pa in [0x1000_0000usize, 0x4000_0000, 0x8000_0000, 0x2_4000_0000] {
            assert!(root_slot(pa) < USER_WINDOW_SLOT, "{pa:#x}");
        }
    }

    #[test]
    fn user_pages_in_the_window_are_accepted() {
        assert_eq!(check_user_page(USER_WINDOW_BASE, USER_RX), Ok(()));
        assert_eq!(
            check_user_page(USER_WINDOW_END - PAGE_SIZE, USER_RW),
            Ok(())
        );
    }

    #[test]
    fn unaligned_or_outside_vas_are_refused() {
        assert_eq!(
            check_user_page(USER_WINDOW_BASE + 8, USER_RW),
            Err(UspaceError::Unaligned)
        );
        assert_eq!(
            check_user_page(USER_WINDOW_END, USER_RW),
            Err(UspaceError::OutsideWindow)
        );
        assert_eq!(
            check_user_page(0x8000_0000, USER_RW),
            Err(UspaceError::OutsideWindow)
        );
    }

    #[test]
    fn kernel_global_or_branch_flags_are_refused() {
        // KERNEL_RW: no USER bit, and GLOBAL.
        assert_eq!(
            check_user_page(USER_WINDOW_BASE, KERNEL_RW),
            Err(UspaceError::BadFlags)
        );
        let global_user = USER_RW.union(PteFlags::GLOBAL);
        assert_eq!(
            check_user_page(USER_WINDOW_BASE, global_user),
            Err(UspaceError::BadFlags)
        );
        // VALID|USER without R/W/X is a branch pointer, not a leaf.
        let branch = PteFlags::VALID.union(PteFlags::USER);
        assert_eq!(
            check_user_page(USER_WINDOW_BASE, branch),
            Err(UspaceError::BadFlags)
        );
    }

    #[test]
    fn write_without_read_and_write_plus_execute_are_refused() {
        let user = PteFlags::VALID.union(PteFlags::USER);
        let write_only = user.union(PteFlags::WRITE);
        assert_eq!(
            check_user_page(USER_WINDOW_BASE, write_only),
            Err(UspaceError::BadFlags)
        );
        let rwx = USER_RW.union(PteFlags::EXECUTE);
        assert_eq!(
            check_user_page(USER_WINDOW_BASE, rwx),
            Err(UspaceError::BadFlags)
        );
    }

    #[test]
    fn every_slot_but_the_window_is_the_kernels() {
        let kernel = (0..PT_ENTRIES).filter(|&i| is_kernel_slot(i)).count();
        assert_eq!(kernel, PT_ENTRIES - 1);
        assert!(!is_kernel_slot(USER_WINDOW_SLOT));
        assert!(!is_kernel_slot(PT_ENTRIES));
    }
}
