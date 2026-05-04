// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel-side driver-manifest verification.
//!
//! The PARSER lives in `wari_driver_iface::parse` so the kernel
//! and the host-side sign tool share one implementation. This
//! module adds the kernel-only piece: `verify_exports`, which
//! resolves every manifest-declared export against the live wasmi
//! `Instance` and asserts the typed signature matches.
//!
//! See `docs/driver-interface-design.md` §5.

pub use wari_driver_iface::parse::{parse_from_wasm, DriverManifestView};

use wari_driver_iface::FuncSig;

/// Resolve every export the manifest declares against the live
/// wasmi instance, asserting the declared signature matches what
/// wasmi typed it as. Called by the loader after instantiate but
/// before any export is invoked. Catches signature drift the sign
/// tool would normally have caught: manifest claims `write:
/// (u32,u32) -> i32` but the binary exposes `write: (u32) -> i32`.
pub fn verify_exports<S>(
    view: &DriverManifestView<'_>,
    instance: &wasmi::Instance,
    store: &S,
) -> Result<(), crate::error::KernelError>
where
    S: wasmi::AsContext,
{
    use crate::error::KernelError;

    for export in view.exports {
        let name = wari_driver_iface::parse::trim_nul(&export.name);
        let sig = FuncSig::from_raw(export.sig)
            .ok_or(KernelError::DriverManifestMalformed)?;
        if !export_matches(instance, store, name, sig) {
            return Err(KernelError::DriverBadExport);
        }
    }
    Ok(())
}

fn export_matches<S>(
    instance: &wasmi::Instance,
    store: &S,
    name: &[u8],
    sig: FuncSig,
) -> bool
where
    S: wasmi::AsContext,
{
    let Ok(name_str) = core::str::from_utf8(name) else {
        return false;
    };
    match sig {
        FuncSig::UnitUnit => instance
            .get_typed_func::<(), ()>(store, name_str)
            .is_ok(),
        FuncSig::U32xU32I32 => instance
            .get_typed_func::<(u32, u32), i32>(store, name_str)
            .is_ok(),
        FuncSig::U32I32 => instance
            .get_typed_func::<u32, i32>(store, name_str)
            .is_ok(),
        FuncSig::U32U32 => instance
            .get_typed_func::<u32, u32>(store, name_str)
            .is_ok(),
        FuncSig::U64I32 => instance
            .get_typed_func::<u64, i32>(store, name_str)
            .is_ok(),
        FuncSig::UnitU64 => instance
            .get_typed_func::<(), u64>(store, name_str)
            .is_ok(),
        FuncSig::U32x5I32 => instance
            .get_typed_func::<(u32, u32, u32, u32, u32), i32>(
                store, name_str,
            )
            .is_ok(),
        FuncSig::U32x3I32 => instance
            .get_typed_func::<(u32, u32, u32), i32>(store, name_str)
            .is_ok(),
    }
}
