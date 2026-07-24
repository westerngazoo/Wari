//! Minimal static RV64 Linux ELF writer for the G4 spike harness.
//!
//! **Throwaway.** Wari never loads an ELF (R7); this file exists only so the
//! Cranelift-emitted machine code can be executed *off* Wari — on the VF2's
//! Debian riscv64 install (Phase B in `docs/aot-spike-results.md`) — to get a
//! real native wall-clock number.
//!
//! Written by hand rather than via a linker because the pinned toolchain has
//! no RISC-V assembler or linker driver on this host: `rust-lld` exists but
//! Cranelift's raw code buffer is not an object file, and no
//! `riscv64-*-gcc`/`llvm-mc` ships with `llvm-tools-preview`. Emitting the
//! ~120 bytes of ELF headers directly is smaller than any alternative and is
//! trivially deterministic (R8): the output has no timestamps, no paths and
//! no toolchain fingerprints.
//!
//! Every hand-encoded instruction below was cross-checked against
//! `llvm-mc --triple=riscv64 --show-encoding`, and the finished ELF is
//! disassembled with `llvm-objdump -d` as the acceptance check.

/// Load address of the image. Well clear of the Linux `mmap_min_addr` floor.
const BASE: u64 = 0x1_0000;
/// Virtual address of the harness's zero-filled linear-memory stand-in.
const MEM_VADDR: u64 = 0x2_0000;
/// Size of that region — one wasm page short of nothing useful; 64 KiB is the
/// `(memory 1)` the `memory.wat` fixture declares.
const MEM_LEN: u64 = 65536;

/// `e_flags` = `EF_RISCV_RVC | EF_RISCV_FLOAT_ABI_DOUBLE`. Cranelift's riscv64
/// backend targets lp64d and emits a 2-byte `unimp` for traps.
const E_FLAGS: u32 = 0x0000_0005;

const EHDR_LEN: u64 = 64;
const PHDR_LEN: u64 = 56;
const SHDR_LEN: u64 = 64;
const PHDR_COUNT: u64 = 2;
const SHDR_COUNT: u64 = 3;

// RV64 register numbers used by the harness.
const REG_ZERO: u32 = 0;
const REG_RA: u32 = 1;
const REG_SP: u32 = 2;
const REG_S1: u32 = 9;
const REG_A0: u32 = 10;
const REG_A1: u32 = 11;
const REG_A2: u32 = 12;
const REG_A7: u32 = 17;

// Linux riscv64 syscall numbers.
const SYS_WRITE: i32 = 64;
const SYS_EXIT: i32 = 93;

/// Encodes `addi rd, rs1, imm` (also `li rd, imm` when `rs1 == x0`).
fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
    (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | 0x13
}

/// Encodes `lui rd, imm20`.
fn lui(rd: u32, imm20: u32) -> u32 {
    ((imm20 & 0xf_ffff) << 12) | (rd << 7) | 0x37
}

/// Encodes `sw rs2, imm(rs1)`.
fn sw(rs2: u32, rs1: u32, imm: i32) -> u32 {
    let imm = imm as u32;
    (((imm >> 5) & 0x7f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (0b010 << 12)
        | ((imm & 0x1f) << 7)
        | 0x23
}

/// Encodes `bne rs1, rs2, offset`.
fn bne(rs1: u32, rs2: u32, off: i32) -> u32 {
    let o = off as u32;
    (((o >> 12) & 1) << 31)
        | (((o >> 5) & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (0b001 << 12)
        | (((o >> 1) & 0xf) << 8)
        | (((o >> 11) & 1) << 7)
        | 0x63
}

/// Encodes `jal rd, offset`.
fn jal(rd: u32, off: i32) -> u32 {
    let o = off as u32;
    (((o >> 20) & 1) << 31)
        | (((o >> 1) & 0x3ff) << 21)
        | (((o >> 11) & 1) << 20)
        | (((o >> 12) & 0xff) << 12)
        | (rd << 7)
        | 0x6f
}

/// `ecall`.
const ECALL: u32 = 0x0000_0073;

/// Materialises a 32-bit constant into `rd` (`lui` + `addi`, or one `addi`).
fn li32(rd: u32, imm: i64) -> Vec<u32> {
    if (-2048..2048).contains(&imm) {
        return vec![addi(rd, REG_ZERO, imm as i32)];
    }
    let lo = ((imm << 52) >> 52) as i32; // sign-extended low 12 bits
    let hi = ((imm - i64::from(lo)) >> 12) as u32;
    vec![lui(rd, hi), addi(rd, rd, lo)]
}

/// Builds the `_start` harness, returning `(instructions, jal_index)`.
///
/// The harness calls the compiled function `repeat` times with
/// `a0 = MEM_VADDR`, `a1 = MEM_LEN`, writes the final `i32` result to fd 1 as
/// 4 little-endian bytes, then exits 0.
fn shim(repeat: u32) -> (Vec<u32>, usize) {
    let mut code = Vec::new();
    code.push(addi(REG_SP, REG_SP, -16));
    code.extend(li32(REG_S1, i64::from(repeat)));
    let loop_top = code.len();
    code.extend(li32(REG_A0, MEM_VADDR as i64));
    code.extend(li32(REG_A1, MEM_LEN as i64));
    let jal_idx = code.len();
    code.push(jal(REG_RA, 0)); // patched by `build`
    code.push(addi(REG_S1, REG_S1, -1));
    let back = -(((code.len() - loop_top) as i32) * 4);
    code.push(bne(REG_S1, REG_ZERO, back));
    code.push(sw(REG_A0, REG_SP, 8)); // stash the result
    code.push(addi(REG_A0, REG_ZERO, 1)); // fd = stdout
    code.push(addi(REG_A1, REG_SP, 8)); // buf = &result
    code.push(addi(REG_A2, REG_ZERO, 4)); // len = 4
    code.push(addi(REG_A7, REG_ZERO, SYS_WRITE));
    code.push(ECALL);
    code.push(addi(REG_A0, REG_ZERO, 0));
    code.push(addi(REG_A7, REG_ZERO, SYS_EXIT));
    code.push(ECALL);
    (code, jal_idx)
}

/// Layout facts about a built image, for reporting.
pub struct Image {
    /// The complete ELF file bytes.
    pub bytes: Vec<u8>,
    /// Entry point virtual address.
    pub entry: u64,
    /// Virtual address of the compiled wasm function.
    pub func_vaddr: u64,
    /// Byte length of the `_start` harness.
    pub shim_len: usize,
}

/// Builds a statically-linked, freestanding RV64 Linux executable that runs
/// `code` (a Cranelift code buffer for a `fn(i64, i64) -> i32`).
///
/// # Panics
/// Never — all arithmetic is on bounded, locally-derived offsets.
pub fn build(code: &[u8], repeat: u32) -> Image {
    let (mut shim_words, jal_idx) = shim(repeat);
    let shim_len = shim_words.len() * 4;
    // The compiled function sits immediately after the harness, 4-aligned by
    // construction (every harness instruction is 4 bytes).
    let jal_off = (shim_len - jal_idx * 4) as i32;
    shim_words[jal_idx] = jal(REG_RA, jal_off);

    let mut text = Vec::with_capacity(shim_len + code.len());
    for w in &shim_words {
        text.extend_from_slice(&w.to_le_bytes());
    }
    text.extend_from_slice(code);

    let text_off = EHDR_LEN + PHDR_LEN * PHDR_COUNT;
    let text_vaddr = BASE + text_off;
    let shstrtab: &[u8] = b"\0.text\0.shstrtab\0";
    let shstrtab_off = text_off + text.len() as u64;
    let shoff = (shstrtab_off + shstrtab.len() as u64).next_multiple_of(8);
    let total = shoff + SHDR_LEN * SHDR_COUNT;

    let mut out = Vec::with_capacity(total as usize);

    // --- ELF header ------------------------------------------------------
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]); // ELF64 LE SYSV
    out.extend_from_slice(&[0; 8]); // e_ident padding
    out.extend_from_slice(&2u16.to_le_bytes()); // e_type   = ET_EXEC
    out.extend_from_slice(&243u16.to_le_bytes()); // e_machine = EM_RISCV
    out.extend_from_slice(&1u32.to_le_bytes()); // e_version
    out.extend_from_slice(&text_vaddr.to_le_bytes()); // e_entry
    out.extend_from_slice(&EHDR_LEN.to_le_bytes()); // e_phoff
    out.extend_from_slice(&shoff.to_le_bytes()); // e_shoff
    out.extend_from_slice(&E_FLAGS.to_le_bytes()); // e_flags
    out.extend_from_slice(&(EHDR_LEN as u16).to_le_bytes()); // e_ehsize
    out.extend_from_slice(&(PHDR_LEN as u16).to_le_bytes()); // e_phentsize
    out.extend_from_slice(&(PHDR_COUNT as u16).to_le_bytes()); // e_phnum
    out.extend_from_slice(&(SHDR_LEN as u16).to_le_bytes()); // e_shentsize
    out.extend_from_slice(&(SHDR_COUNT as u16).to_le_bytes()); // e_shnum
    out.extend_from_slice(&2u16.to_le_bytes()); // e_shstrndx

    // --- program headers -------------------------------------------------
    // PT_LOAD #0: headers + .text, R+X. Never W+X — the harness honours D4
    // even though it runs on Linux, not Wari.
    push_phdr(&mut out, 0, BASE, total, total, 0b101, 0x1000);
    // PT_LOAD #1: the zero-filled linear-memory stand-in, R+W, filesz 0.
    push_phdr(&mut out, 0, MEM_VADDR, 0, MEM_LEN, 0b110, 0x1000);

    debug_assert_eq!(out.len() as u64, text_off);
    out.extend_from_slice(&text);
    out.extend_from_slice(shstrtab);
    while !(out.len() as u64).is_multiple_of(8) {
        out.push(0);
    }

    // --- section headers -------------------------------------------------
    push_shdr(&mut out, 0, 0, 0, 0, 0, 0, 0); // SHN_UNDEF
    push_shdr(
        &mut out,
        1,     // name = ".text"
        1,     // SHT_PROGBITS
        0b110, // SHF_ALLOC | SHF_EXECINSTR
        text_vaddr,
        text_off,
        text.len() as u64,
        4,
    );
    push_shdr(
        &mut out,
        7, // name = ".shstrtab"
        3, // SHT_STRTAB
        0,
        0,
        shstrtab_off,
        shstrtab.len() as u64,
        1,
    );

    Image {
        entry: text_vaddr,
        func_vaddr: text_vaddr + shim_len as u64,
        shim_len,
        bytes: out,
    }
}

/// Appends one `Elf64_Phdr`.
fn push_phdr(
    out: &mut Vec<u8>,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
    align: u64,
) {
    out.extend_from_slice(&1u32.to_le_bytes()); // p_type = PT_LOAD
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&vaddr.to_le_bytes());
    out.extend_from_slice(&vaddr.to_le_bytes()); // p_paddr
    out.extend_from_slice(&filesz.to_le_bytes());
    out.extend_from_slice(&memsz.to_le_bytes());
    out.extend_from_slice(&align.to_le_bytes());
}

/// Appends one `Elf64_Shdr`.
#[allow(clippy::too_many_arguments)]
fn push_shdr(
    out: &mut Vec<u8>,
    name: u32,
    sh_type: u32,
    flags: u64,
    addr: u64,
    offset: u64,
    size: u64,
    align: u64,
) {
    out.extend_from_slice(&name.to_le_bytes());
    out.extend_from_slice(&sh_type.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&addr.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // sh_link
    out.extend_from_slice(&0u32.to_le_bytes()); // sh_info
    out.extend_from_slice(&align.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // sh_entsize
}

// clippy::{panic,expect_used} allowed: test code, where a failed
// precondition should abort the test loudly.
#[cfg(test)]
#[allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Encodings cross-checked against
    /// `llvm-mc --triple=riscv64 --mattr=+m,+a,+f,+d --show-encoding`.
    #[test]
    fn instruction_encodings_match_llvm_mc() {
        assert_eq!(addi(REG_SP, REG_SP, -16), 0xff01_0113);
        assert_eq!(addi(REG_A0, REG_ZERO, 1), 0x0010_0513);
        assert_eq!(addi(REG_A2, REG_ZERO, 4), 0x0040_0613);
        assert_eq!(addi(REG_A1, REG_SP, 8), 0x0081_0593);
        assert_eq!(addi(REG_A7, REG_ZERO, SYS_WRITE), 0x0400_0893);
        assert_eq!(addi(REG_A7, REG_ZERO, SYS_EXIT), 0x05d0_0893);
        assert_eq!(sw(REG_A0, REG_SP, 8), 0x00a1_2423);
        assert_eq!(ECALL, 0x0000_0073);
    }

    #[test]
    fn header_is_a_riscv64_executable() {
        let img = build(&[0x67, 0x80, 0x00, 0x00], 1); // `ret`
        assert_eq!(&img.bytes[0..4], b"\x7fELF");
        assert_eq!(img.bytes[4], 2); // ELFCLASS64
        assert_eq!(u16::from_le_bytes([img.bytes[18], img.bytes[19]]), 243); // EM_RISCV
        assert_eq!(img.entry, BASE + EHDR_LEN + PHDR_LEN * PHDR_COUNT);
        assert_eq!(img.func_vaddr, img.entry + img.shim_len as u64);
    }
}
