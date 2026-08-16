// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-cap`'s capability grant specs.
//!
//! The attenuation logic (`requested & ceiling`) and its host tests
//! live in the pure `wari-cap` crate (`grants` module). This shim
//! surfaces `GrantSpec` under `crate::cap::grants` so the kernel's
//! spawn/install paths consume it without importing the crate name
//! directly — the same pattern every other extracted cap module uses.
#[allow(unused_imports)]
pub use wari_cap::grants::*;
