// SPDX-License-Identifier: AGPL-3.0-only
//! `KernelError` — the single error taxonomy for the kernel (CLAUDE R5).
//!
//! Every fallible operation inside Tier 0 returns `Result<T, KernelError>`.
//! Panics are last-resort only, with a justifying comment.
//!
//! `KernelError` differs from `wari_abi::SyscallError`: the ABI error
//! is the userspace-visible encoding (fits in `a0`). `KernelError` is
//! the internal richer enum, converted at the syscall boundary. This
//! separation lets the kernel distinguish "target process is in state X"
//! from "out of physical pages" — details userspace doesn't need.
//!
//! Extracted from `kernel/src/error.rs` (host-testing program,
//! `docs/kernel-host-testing-design.md`); the kernel re-exports it
//! via a shim so `crate::error::KernelError` paths are unchanged.

#![cfg_attr(not(test), no_std)]

/// Internal kernel result type. Mapped to `wari_abi::SyscallError` at
/// the syscall boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelError {
    /// An argument was out of range or malformed.
    InvalidArgument,
    /// Target PID does not exist or is not in the expected state.
    NoSuchProcess,
    /// Caller does not hold the capability required for this operation.
    PermissionDenied,
    /// Operation would block; caller did not opt into blocking.
    WouldBlock,
    /// Out of physical pages.
    OutOfPages,
    /// Out of handles in a fixed pool (sockets, caps, etc.).
    OutOfHandles,
    /// Page/handle/capability not mapped or not owned by caller.
    NotMapped,
    /// WASM module failed validation at load time.
    BadWasm,
    /// Driver-layer failure — see driver-specific log line for detail.
    DriverError,
    /// Tier-2 driver binary has no `wari_driver_manifest` custom
    /// section, or it is malformed (bad magic, truncated, unknown
    /// signature discriminant). The driver is not loadable.
    DriverManifestMalformed,
    /// Driver manifest carries an `abi_version` the kernel does not
    /// support. Driver was built against a newer (or older)
    /// `wari-driver-iface` than this kernel knows.
    DriverAbiVersion,
    /// Driver manifest declares a `kind` other than the slot the
    /// kernel is loading the binary into (e.g. `Net` binary in the
    /// `Uart` slot). Refused before any code runs.
    DriverWrongKind,
    /// Driver manifest declares an export that the WASM does not
    /// actually expose, or exposes with a different signature than
    /// the manifest claims. The sign tool should have caught this
    /// (PR DI-5); the kernel double-checks at load time.
    DriverBadExport,
    /// Driver manifest declares a host fn import the kernel did not
    /// register on the linker. Driver expects a surface the kernel
    /// does not provide.
    DriverMissingHostFn,
}

impl From<wari_driver_iface::DriverManifestError> for KernelError {
    fn from(e: wari_driver_iface::DriverManifestError) -> Self {
        use wari_driver_iface::DriverManifestError as M;
        match e {
            M::Missing | M::Truncated | M::BadMagic | M::UnknownSig => {
                KernelError::DriverManifestMalformed
            }
            M::UnsupportedAbiVersion => KernelError::DriverAbiVersion,
            M::UnknownKind | M::WrongKind => KernelError::DriverWrongKind,
            M::ExportMismatch => KernelError::DriverBadExport,
            M::MissingHostFn => KernelError::DriverMissingHostFn,
        }
    }
}

impl KernelError {
    /// Convert to the userspace-visible `SyscallError`.
    ///
    /// Multiple kernel errors may collapse to the same user error —
    /// userspace rarely needs the internal distinction, and collapsing
    /// limits information leakage across the trust boundary.
    pub const fn into_syscall(self) -> wari_abi::SyscallError {
        use wari_abi::SyscallError as E;
        match self {
            KernelError::InvalidArgument => E::InvalidArgument,
            KernelError::NoSuchProcess => E::NoSuchProcess,
            KernelError::PermissionDenied => E::PermissionDenied,
            KernelError::WouldBlock => E::WouldBlock,
            KernelError::OutOfPages => E::OutOfResources,
            KernelError::OutOfHandles => E::OutOfResources,
            KernelError::NotMapped => E::NotMapped,
            KernelError::BadWasm => E::BadWasm,
            KernelError::DriverError => E::Generic,
            KernelError::DriverManifestMalformed
            | KernelError::DriverAbiVersion
            | KernelError::DriverWrongKind
            | KernelError::DriverBadExport
            | KernelError::DriverMissingHostFn => E::BadWasm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_manifest_errors_map_to_driver_variants() {
        use wari_driver_iface::DriverManifestError as M;
        assert_eq!(
            KernelError::from(M::BadMagic),
            KernelError::DriverManifestMalformed
        );
        assert_eq!(
            KernelError::from(M::UnsupportedAbiVersion),
            KernelError::DriverAbiVersion
        );
        assert_eq!(
            KernelError::from(M::WrongKind),
            KernelError::DriverWrongKind
        );
        assert_eq!(
            KernelError::from(M::ExportMismatch),
            KernelError::DriverBadExport
        );
        assert_eq!(
            KernelError::from(M::MissingHostFn),
            KernelError::DriverMissingHostFn
        );
    }

    #[test]
    fn into_syscall_collapses_internal_detail() {
        use wari_abi::SyscallError as E;
        // Distinct internal exhaustion reasons collapse to one user
        // error — the trust boundary leaks no pool identity.
        assert_eq!(KernelError::OutOfPages.into_syscall(), E::OutOfResources);
        assert_eq!(KernelError::OutOfHandles.into_syscall(), E::OutOfResources);
        // Driver-load failures all surface as BadWasm to userspace.
        assert_eq!(
            KernelError::DriverManifestMalformed.into_syscall(),
            E::BadWasm
        );
        assert_eq!(
            KernelError::PermissionDenied.into_syscall(),
            E::PermissionDenied
        );
    }
}
