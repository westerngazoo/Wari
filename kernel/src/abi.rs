//! Re-exports the kernel-facing slice of `wari-abi`.
//!
//! The ABI is defined once in the `wari-abi` workspace crate (so WASM
//! tooling and the kernel share one source of truth — see CLAUDE R8).
//! This module exists because the kernel wants to write `crate::abi::*`
//! not `wari_abi::*` internally.
//!
//! Phase 0: empty re-exports. Phase 0a cherry-picks `SYS_*` constants
//! and `SyscallError` from `goose-os/kernel/src/abi.rs` into
//! `wari-abi/src/lib.rs`; this module then becomes `pub use wari_abi::*;`.

#[allow(unused_imports)] // Phase 0: re-export for kernel modules; consumers arrive PR 2+.
pub use wari_abi::*;
