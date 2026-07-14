// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-sched`'s process state machine.
//!
//! The pure logic (and host tests) live in the `wari-sched`
//! workspace crate — lane B-1 of the extraction program in
//! `docs/kernel-host-testing-design.md`. This kernel-side module
//! exists only so existing call sites using
//! `crate::sched::process::{Process, ProcessState, MsgRegs, …}` keep
//! compiling unchanged — the same shim pattern as
//! `mem/page_alloc.rs`.

#[allow(unused_imports)]
pub use wari_sched::process::*;
