// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a Tier-2 driver cannot aim the DMA engine outside its
//! own linear memory (INV-25, ADR-002 step 1).
//!
//! ## What this verifies
//!
//! The net driver no longer forms physical addresses. It names a
//! buffer by linear-memory offset through `wari::nic_dma_set_buf`,
//! and the kernel (`cap::dma::nic_dma_set_buf_impl`) validates the
//! span and the descriptor index before writing the address into the
//! descriptor, which lives in kernel memory. During `init_virtio` the
//! real driver makes nine attempts that must be refused and logs the
//! kernel's answer to each: a span that starts past the end of memory
//! (`DMAo`), a descriptor index past the ring (`DMAi`), a span whose
//! length runs off the end (`DMAl`), raising the queue size register
//! (`DMAq`), accepting a feature that changes the ring layout
//! (`DMAf`) and the four GMAC ring operations on this VirtIO board
//! (`DMAg`, `DMAh`, `DMAj`, `DMAu`).
//! If any were accepted the driver refuses to bring the
//! device up, so the network coming up is itself evidence that all
//! nine refusals happened.
//!
//! The pure decisions (`wari_validate::dma`) carry their own truth
//! tables on the host; this test proves the boundary end to end on
//! the real driver under QEMU, with no bespoke blob: the negative
//! calls go through the same imports, cap check and linear memory the
//! positive path uses one line later.
//!
//! Not covered here (VF2 follow-up): the GMAC path still computes its
//! ring and buffer addresses from `lin_mem_base`.

use wari_security_tests::{boot_kernel_capture_with_net, markers, DEFAULT_WALLCLOCK};

/// `[net:drv] tag=0x444d416f` — 'DMAo', span outside linear memory.
const TAG_DMA_OOB: &str = "tag=0x444d416f";
/// `[net:drv] tag=0x444d4169` — 'DMAi', descriptor index past the ring.
const TAG_DMA_IDX: &str = "tag=0x444d4169";
/// `[net:drv] tag=0x444d416c` — 'DMAl', span that starts inside
/// linear memory and whose length runs past its end.
const TAG_DMA_LEN: &str = "tag=0x444d416c";
/// `[net:drv] tag=0x444d4171` — 'DMAq', a driver write to the
/// queue-size register (the device would walk kernel memory past the
/// descriptor table).
const TAG_DMA_QNUM: &str = "tag=0x444d4171";
/// `[net:drv] tag=0x444d4166` — 'DMAf', accepting a feature that
/// changes the ring layout the kernel validated.
const TAG_DMA_FEAT: &str = "tag=0x444d4166";
/// `[net:drv] tag=0x444d4167` — 'DMAg', attaching a GMAC ring on a
/// board whose NIC is VirtIO.
const TAG_DMA_GMAC: &str = "tag=0x444d4167";
/// 'DMAh' / 'DMAj' / 'DMAu' — the other GMAC ring operations (kick,
/// rewind, status) on the same VirtIO board.
const TAG_DMA_GKICK: &str = "tag=0x444d4168";
const TAG_DMA_GREWIND: &str = "tag=0x444d416a";
const TAG_DMA_GSTATUS: &str = "tag=0x444d4175";
/// `cap::syscall::E_INVAL` (-2) as the kernel prints it.
const E_INVAL_HEX: &str = "val=0xfffffffe";

#[test]
fn dma_outside_linmem_is_refused() {
    let text = boot_kernel_capture_with_net(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    for tag in [
        TAG_DMA_OOB,
        TAG_DMA_IDX,
        TAG_DMA_LEN,
        TAG_DMA_QNUM,
        TAG_DMA_FEAT,
        TAG_DMA_GMAC,
        TAG_DMA_GKICK,
        TAG_DMA_GREWIND,
        TAG_DMA_GSTATUS,
    ] {
        let line = text
            .lines()
            .find(|l| l.contains(tag))
            .unwrap_or_else(|| panic!("self-check {tag} never logged:\n{text}"));
        assert!(
            line.contains(E_INVAL_HEX),
            "kernel accepted a DMA span it must refuse ({tag}): {line}\n{text}",
        );
    }

    // The driver only proceeds past the self-check when both calls
    // were refused, so the stack coming up is the positive arm: the
    // legitimate `nic_dma_set_buf` calls in populate_rx were accepted.
    assert!(
        text.contains(markers::NET_UP),
        "net stack did not come up after the INV-25 self-check:\n{text}",
    );

    assert!(
        !text.contains("panicked at"),
        "rust panic detected in dma_escape run:\n{text}",
    );
}
