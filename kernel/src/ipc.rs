// SPDX-License-Identifier: AGPL-3.0-only
//! seL4-style synchronous IPC — the impure glue around
//! `wari_ipc::resolve` (Option B, brick 3b).
//!
//! Layering (docs/ipc-design.md):
//! - **Decision plane**: `wari_ipc::resolve(op, peer_waiting)` — the
//!   pure, host-tested rendezvous state machine (crate `wari-ipc`).
//! - **Data plane**: `sched::process::transfer_msg` — pure copy of
//!   `MsgRegs` (badge + 4 words).
//! - **This file**: the mechanism those planes drive — Endpoint cap
//!   checks, sender/receiver queues, `Blocked` transitions, linear-
//!   memory marshaling, wake + resume-value plumbing, and the
//!   `IpcBlock` yield that suspends the calling tenant
//!   (`runtime::tier1_pool`'s protocol).
//!
//! ## Message marshaling — who touches whose memory
//!
//! A message on the WASM side is `wari_abi::net::IPC_MSG_BYTES` (40)
//! bytes in the caller's linear memory: `badge u64 | words [u64; 4]`,
//! little-endian. The kernel NEVER writes another instance's linear
//! memory from inside a host fn (that would alias the peer's `Store`
//! while wasmi may hold it). Instead:
//!
//! - A **running** caller marshals through its own `Caller` memory.
//! - A **blocked** peer receives kernel-side only: the rendezvous
//!   copies into its `Process::msg_regs` and records nothing else;
//!   the scheduler flushes `msg_regs → linmem[msg_buf]` just before
//!   resuming it (`runtime::tier1_pool::flush_msg_to_linmem`), when
//!   no wasmi frame can possibly hold that instance's store.
//!
//! ## Reply partners
//!
//! A caller awaiting its reply (`Blocked { ReplyWait, ep }`) is NOT
//! queued on the endpoint — seL4 models this with a one-shot reply
//! cap. Phase-2 minimal: `reply` scans the process table for the
//! lowest-pid `ReplyWait` waiter on that endpoint. Correct for the
//! Phase-2 workloads (one caller per endpoint at a time); the reply-
//! cap object replaces the scan when multi-caller endpoints arrive.

#![allow(dead_code)]

use wasmi::Caller;

use crate::cap::syscall::{check_cap, E_AGAIN, E_INVAL, E_NOMEM, E_PERM};
use crate::cap::{
    cspaces, object_pools, ObjectKind, TcbRef, CAP_RIGHT_READ, CAP_RIGHT_WRITE,
    CSPACE_SLOTS, MAX_PROCS,
};
use crate::runtime::tier1_pool::IpcBlock;
use crate::runtime::wasi::Tier1HostState;
use crate::sched::{self, process::transfer_msg, process::BlockReason, process::MsgRegs};
use wari_ipc::{resolve, CallerNext, Op, Outcome};

/// Wire size of one message in linear memory (mirrors `wari-abi`).
const MSG_BYTES: usize = wari_abi::net::IPC_MSG_BYTES;

/// Decode a 40-byte little-endian buffer into `MsgRegs`.
fn decode_msg(buf: &[u8; MSG_BYTES]) -> MsgRegs {
    let mut words = [0u64; 4];
    let badge = u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let mut i = 0;
    while i < 4 {
        let o = 8 + i * 8;
        words[i] = u64::from_le_bytes([
            buf[o], buf[o + 1], buf[o + 2], buf[o + 3],
            buf[o + 4], buf[o + 5], buf[o + 6], buf[o + 7],
        ]);
        i += 1;
    }
    MsgRegs { badge, words }
}

/// Encode `MsgRegs` into the 40-byte little-endian wire form.
pub fn encode_msg(regs: &MsgRegs) -> [u8; MSG_BYTES] {
    let mut out = [0u8; MSG_BYTES];
    out[0..8].copy_from_slice(&regs.badge.to_le_bytes());
    let mut i = 0;
    while i < 4 {
        let o = 8 + i * 8;
        out[o..o + 8].copy_from_slice(&regs.words[i].to_le_bytes());
        i += 1;
    }
    out
}

/// Resolve the caller's Endpoint cap at `slot`, returning the
/// endpoint pool index. `required_rights` follows the send/recv
/// asymmetry: WRITE to deliver into an endpoint, READ to take from
/// it (mirrors the UART endpoint convention in `cap::boot`).
fn resolve_endpoint(proc_id: u8, slot: u32, required_rights: u8) -> Result<u16, i32> {
    if slot >= CSPACE_SLOTS as u32 {
        return Err(E_INVAL);
    }
    check_cap(proc_id, slot as u8, ObjectKind::Endpoint, required_rights)?;
    let cs = cspaces();
    Ok(cs[proc_id as usize].slots[slot as usize].pool_index)
}

/// Read the caller's 40-byte message from its own linear memory.
fn read_msg(
    caller: &mut Caller<'_, Tier1HostState>,
    msg_ptr: u32,
) -> Result<MsgRegs, i32> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or(E_NOMEM)?;
    let mut buf = [0u8; MSG_BYTES];
    memory
        .read(&*caller, msg_ptr as usize, &mut buf)
        .map_err(|_| E_INVAL)?;
    Ok(decode_msg(&buf))
}

/// Write a message into the caller's own linear memory.
fn write_msg(
    caller: &mut Caller<'_, Tier1HostState>,
    msg_ptr: u32,
    regs: &MsgRegs,
) -> Result<(), i32> {
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or(E_NOMEM)?;
    memory
        .write(&mut *caller, msg_ptr as usize, &encode_msg(regs))
        .map_err(|_| E_INVAL)
}

/// Block the calling process on `ep_idx` and yield to the scheduler.
/// Records `msg_ptr` so the resume-time flush knows where to deliver
/// an incoming message (recv/call) — harmless for send (nothing is
/// flushed unless a message was transferred in).
fn block_and_yield(
    proc_id: u8,
    reason: BlockReason,
    ep_idx: u16,
    msg_ptr: u32,
) -> wasmi::Error {
    let table = sched::processes();
    if let Some(p) = table[proc_id as usize].as_mut() {
        p.msg_buf = msg_ptr;
        p.block(reason, ep_idx as u8);
    }
    wasmi::Error::host(IpcBlock)
}

/// Wake a blocked peer with syscall result `rc`, transferring `msg`
/// into its kernel-side registers first (flushed to its linear
/// memory by the scheduler at resume). `msg = None` wakes without
/// delivering (e.g. a sender whose message was already taken).
fn deliver_and_wake(peer: u8, msg: Option<&MsgRegs>, rc: i32) {
    let table = sched::processes();
    if let Some(p) = table[peer as usize].as_mut() {
        if let Some(m) = msg {
            transfer_msg(m, &mut p.msg_regs);
        }
    }
    let _ = crate::runtime::tier1_pool::set_resume_value(peer, rc);
    let _ = sched::wake(peer);
}

/// Find the lowest-pid process in `Blocked { ReplyWait, ep_idx }`.
fn reply_waiter_on(ep_idx: u16) -> Option<u8> {
    let table = sched::processes();
    for (i, slot) in table.iter().enumerate().take(MAX_PROCS) {
        if let Some(p) = slot {
            if matches!(
                p.state,
                crate::sched::ProcessState::Blocked {
                    reason: BlockReason::ReplyWait,
                    ep_idx: e
                } if e as u16 == ep_idx
            ) {
                return Some(i as u8);
            }
        }
    }
    None
}

/// `wari::ipc_send(slot, msg_ptr) -> i32` — deliver a message;
/// blocks only if no receiver is waiting.
pub fn ipc_send_impl(
    caller: &mut Caller<'_, Tier1HostState>,
    proc_id: u8,
    slot: u32,
    msg_ptr: u32,
) -> Result<i32, wasmi::Error> {
    do_send_like(caller, proc_id, slot, msg_ptr, Op::Send)
}

/// `wari::ipc_call(slot, msg_ptr) -> i32` — send + await the reply.
/// The reply overwrites the same `msg_ptr` buffer (seL4 MR-style
/// in/out). Always ends blocked until replied.
pub fn ipc_call_impl(
    caller: &mut Caller<'_, Tier1HostState>,
    proc_id: u8,
    slot: u32,
    msg_ptr: u32,
) -> Result<i32, wasmi::Error> {
    do_send_like(caller, proc_id, slot, msg_ptr, Op::Call)
}

/// Shared send/call mechanism — they differ only in `Op` and in
/// what the caller does after a rendezvous (`resolve` encodes it).
fn do_send_like(
    caller: &mut Caller<'_, Tier1HostState>,
    proc_id: u8,
    slot: u32,
    msg_ptr: u32,
    op: Op,
) -> Result<i32, wasmi::Error> {
    let ep_idx = match resolve_endpoint(proc_id, slot, CAP_RIGHT_WRITE) {
        Ok(i) => i,
        Err(e) => return Ok(e),
    };
    let msg = match read_msg(caller, msg_ptr) {
        Ok(m) => m,
        Err(e) => return Ok(e),
    };
    // Stash the outbound message kernel-side FIRST: whether we
    // rendezvous or enqueue, the authoritative copy lives in our
    // Process.msg_regs (a queued receiver reads it from there).
    {
        let table = sched::processes();
        if let Some(p) = table[proc_id as usize].as_mut() {
            p.msg_regs = msg;
        }
    }
    let pools = object_pools();
    let peer = pools
        .endpoints
        .get_mut(ep_idx)
        .and_then(|ep| ep.receivers.pop());
    match resolve(op, peer.is_some()) {
        Outcome::Rendezvous { caller: next } => {
            let TcbRef(rx) = match peer {
                Some(t) => t,
                None => return Ok(E_AGAIN), // unreachable; fail closed
            };
            // Receiver is Blocked{RecvWait}: deliver kernel-side;
            // the scheduler flushes to its linmem at resume.
            deliver_and_wake(rx, Some(&msg), 0);
            match next {
                CallerNext::Continue => Ok(0),
                CallerNext::Block(reason) => {
                    // Call: now await the reply (not queued — the
                    // replier finds us via reply_waiter_on).
                    Err(block_and_yield(proc_id, reason, ep_idx, msg_ptr))
                }
            }
        }
        Outcome::Enqueue { block } => {
            let pools = object_pools();
            let pushed = pools
                .endpoints
                .get_mut(ep_idx)
                .map(|ep| ep.senders.push(TcbRef(proc_id)).is_ok())
                .unwrap_or(false);
            if !pushed {
                return Ok(E_AGAIN); // queue full — caller may retry
            }
            Err(block_and_yield(proc_id, block, ep_idx, msg_ptr))
        }
        Outcome::Invalid => Ok(E_INVAL),
    }
}

/// `wari::ipc_recv(slot, msg_ptr) -> i32` — receive a message into
/// `msg_ptr`; blocks if no sender is waiting.
pub fn ipc_recv_impl(
    caller: &mut Caller<'_, Tier1HostState>,
    proc_id: u8,
    slot: u32,
    msg_ptr: u32,
) -> Result<i32, wasmi::Error> {
    let ep_idx = match resolve_endpoint(proc_id, slot, CAP_RIGHT_READ) {
        Ok(i) => i,
        Err(e) => return Ok(e),
    };
    let pools = object_pools();
    let peer = pools
        .endpoints
        .get_mut(ep_idx)
        .and_then(|ep| ep.senders.pop());
    match resolve(Op::Recv, peer.is_some()) {
        Outcome::Rendezvous { .. } => {
            let TcbRef(tx) = match peer {
                Some(t) => t,
                None => return Ok(E_AGAIN), // unreachable; fail closed
            };
            // Take the sender's message (kernel-side authoritative
            // copy) and write it into OUR linear memory — we are the
            // running instance, so our own Caller is the safe path.
            let msg = {
                let table = sched::processes();
                match table[tx as usize].as_ref() {
                    Some(p) => p.msg_regs,
                    None => return Ok(E_PERM),
                }
            };
            if let Err(e) = write_msg(caller, msg_ptr, &msg) {
                return Ok(e);
            }
            // Sender's fate depends on why it waited: a `call`er is
            // promoted to await our reply; a plain `send`er is done.
            let sender_reason = {
                let table = sched::processes();
                table[tx as usize].as_ref().map(|p| p.state)
            };
            match sender_reason {
                Some(crate::sched::ProcessState::Blocked {
                    reason: BlockReason::CallWait,
                    ep_idx: e,
                }) => {
                    let table = sched::processes();
                    if let Some(p) = table[tx as usize].as_mut() {
                        p.block(BlockReason::ReplyWait, e);
                    }
                }
                _ => deliver_and_wake(tx, None, 0),
            }
            Ok(0)
        }
        Outcome::Enqueue { block } => {
            let pools = object_pools();
            let pushed = pools
                .endpoints
                .get_mut(ep_idx)
                .map(|ep| ep.receivers.push(TcbRef(proc_id)).is_ok())
                .unwrap_or(false);
            if !pushed {
                return Ok(E_AGAIN);
            }
            Err(block_and_yield(proc_id, block, ep_idx, msg_ptr))
        }
        Outcome::Invalid => Ok(E_INVAL),
    }
}

/// `wari::ipc_reply(slot, msg_ptr) -> i32` — reply to the caller
/// awaiting on this endpoint. Never blocks; `E_INVAL` if nobody is
/// in `ReplyWait` here (matches `wari_ipc::resolve(Reply, false)`).
pub fn ipc_reply_impl(
    caller: &mut Caller<'_, Tier1HostState>,
    proc_id: u8,
    slot: u32,
    msg_ptr: u32,
) -> Result<i32, wasmi::Error> {
    let ep_idx = match resolve_endpoint(proc_id, slot, CAP_RIGHT_WRITE) {
        Ok(i) => i,
        Err(e) => return Ok(e),
    };
    let waiter = reply_waiter_on(ep_idx);
    match resolve(Op::Reply, waiter.is_some()) {
        Outcome::Rendezvous { .. } => {
            let msg = match read_msg(caller, msg_ptr) {
                Ok(m) => m,
                Err(e) => return Ok(e),
            };
            let rx = match waiter {
                Some(w) => w,
                None => return Ok(E_INVAL), // unreachable; fail closed
            };
            deliver_and_wake(rx, Some(&msg), 0);
            Ok(0)
        }
        Outcome::Invalid => Ok(E_INVAL),
        // resolve(Reply, _) never returns Enqueue; fail closed.
        Outcome::Enqueue { .. } => Ok(E_INVAL),
    }
}
