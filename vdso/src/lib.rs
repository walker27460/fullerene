//! VDSO ELF builder — constructs a shared object at boot time.
//!
//! The builder takes the kernel's physical and virtual addresses for each
//! function to expose, and emits an ELF with one `PT_LOAD` per function page.
//! Each PT_LOAD's `p_vaddr` is a VDSO-assigned virtual address and `p_paddr`
//! is the kernel function's physical address — the kernel maps it identity-style
//! with `USER_ACCESSIBLE`.
//!
//! Userspace links against the VDSO symbols (e.g. `extern "C" { fn vdso_read(...); }`)
//! and calls them as regular functions — no ring buffer, no dispatch table.

#![no_std]

use core::ptr;

// ── Constants ──────────────────────────────────────────────────

/// Base virtual address for VDSO function slots.
/// Each exposed kernel function gets a page-aligned slot starting here.
pub const VDSO_USER_BASE: u64 = 0x7000_0000_0000;

/// Size reserved for VDSO metadata (time_us, uptime_us, pid, padding).
/// Occupies the first page at VDSO_USER_BASE.
pub const VDSO_META_SIZE: usize = 4096;

/// A single entry describing a kernel function to expose through the VDSO.
#[derive(Clone, Copy, Debug)]
pub struct VdsoEntry {
    /// Symbol name that userspace links against (e.g. `"vdso_read"`).
    pub name: &'static str,
    /// Virtual address of the kernel function (in the kernel's address space).
    pub virt_addr: u64,
    /// Physical address of the page containing the function.
    pub phys_addr: u64,
}

// ── ELF builder ───────────────────────────────────────────────

/// Buffer large enough for a VDSO ELF with up to 32 function entries.
/// Header (64) + Phdrs (32 * 56) + symtab data fits easily in 8 pages.
pub const VDSO_BUFFER_SIZE: usize = 32768;

/// Write a complete VDSO ELF into `buf`.
///
/// Returns the length of the ELF data written.
///
/// The resulting ELF has:
/// - One PT_LOAD for VDSO metadata at `VDSO_USER_BASE` (RW)
/// - One PT_LOAD per entry at `VDSO_USER_BASE + VDSO_META_SIZE + index * 4096` (RX)
///
/// The kernel parses the ELF with goblin, finds each PT_LOAD, reads
/// `p_vaddr` and `p_paddr`, and maps `p_paddr → p_vaddr` with `USER_ACCESSIBLE`.
pub fn build(buf: &mut [u8; VDSO_BUFFER_SIZE], entries: &[VdsoEntry]) -> usize {
    let count = entries.len();
    let phnum = 1 + count; // one meta PT_LOAD + one per function

    // Layout: ELF header (64) | Phdr[0..phnum] | symtab strings
    let phoff: u64 = 64;
    let ph_entry_size: u64 = 56;
    let ph_end = phoff + phnum as u64 * ph_entry_size;

    // Symtab: a simple ELF symbol table lives after the program headers.
    // Each slot: st_name (4), st_info (1), st_other (1), st_shndx (2),
    // st_value (8), st_size (8) = 24 bytes.
    // Plus a string table with the symbol names.
    let symtab_off = ph_end as usize;
    let mut str_off = symtab_off + (1 + count) * 24; // NULL symbol + one per function
    let strtab_off = str_off;

    // ── Helper: write a little-endian u64 ───────────────────────
    unsafe fn put_u64(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u64) {
        let p = buf.as_mut_ptr().add(off) as *mut u64;
        ptr::write_unaligned(p, val.to_le());
    }
    unsafe fn put_u32(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u32) {
        let p = buf.as_mut_ptr().add(off) as *mut u32;
        ptr::write_unaligned(p, val.to_le());
    }
    unsafe fn put_u16(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u16) {
        let p = buf.as_mut_ptr().add(off) as *mut u16;
        ptr::write_unaligned(p, val.to_le());
    }
    unsafe fn put_u8(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u8) {
        *buf.get_unchecked_mut(off) = val;
    }

    unsafe {
        // ── ELF header (64 bytes) ──────────────────────────────
        buf[0..4].copy_from_slice(b"\x7fELF");
        put_u8(buf, 4, 2);  // 64-bit
        put_u8(buf, 5, 1);  // little-endian
        put_u8(buf, 6, 1);  // ELF version
        put_u8(buf, 7, 0);  // ELFOSABI_NONE

        put_u16(buf, 16, 3);   // ET_DYN
        put_u16(buf, 18, 62);  // EM_X86_64
        put_u32(buf, 20, 1);   // EV_CURRENT
        put_u64(buf, 24, 0);   // e_entry = 0 (no entry point)
        put_u64(buf, 32, phoff);
        put_u64(buf, 40, symtab_off as u64); // e_shoff — reuse as section header offset
        put_u32(buf, 48, 0);    // e_flags
        put_u16(buf, 52, 64);   // e_ehsize
        put_u16(buf, 54, 56);   // e_phentsize
        put_u16(buf, 56, phnum as u16);
        put_u16(buf, 58, 24);   // e_shentsize (Sym entry = 24 bytes)
        put_u16(buf, 60, (1 + count) as u16); // e_shnum
        put_u16(buf, 62, 1);    // e_shstrndx (first section = symtab, second = strtab)

        // ── Program headers ─────────────────────────────────────
        let mut ph_off = phoff as usize;

        // Phdr[0]: VDSO metadata page (RW)
        put_u32(buf, ph_off, 1);        // PT_LOAD
        ph_off += 4;
        put_u32(buf, ph_off, 6);        // PF_R | PF_W
        ph_off += 4;
        put_u64(buf, ph_off, 0);        // p_offset = 0 (no file data — kernel zeroes)
        ph_off += 8;
        put_u64(buf, ph_off, VDSO_USER_BASE);  // p_vaddr
        ph_off += 8;
        put_u64(buf, ph_off, 0);        // p_paddr = 0 (allocated by kernel)
        ph_off += 8;
        put_u64(buf, ph_off, 4096);     // p_filesz = 0 (no data in file, zero-filled)
        ph_off += 8;
        put_u64(buf, ph_off, 4096);     // p_memsz = 4096
        ph_off += 8;
        put_u64(buf, ph_off, 4096);     // p_align
        ph_off += 8;

        // Phdr[1..1+count]: one per kernel function page (RX)
        for (i, entry) in entries.iter().enumerate() {
            let slot_base = VDSO_USER_BASE + VDSO_META_SIZE as u64 + (i as u64) * 4096;

            put_u32(buf, ph_off, 1);    // PT_LOAD
            ph_off += 4;
            put_u32(buf, ph_off, 5);    // PF_R | PF_X
            ph_off += 4;
            put_u64(buf, ph_off, 0);    // p_offset = 0 (no file data)
            ph_off += 8;
            put_u64(buf, ph_off, slot_base); // p_vaddr — where userspace calls it
            ph_off += 8;
            put_u64(buf, ph_off, entry.phys_addr); // p_paddr — kernel physical page
            ph_off += 8;
            put_u64(buf, ph_off, 4096); // p_filesz = 0
            ph_off += 8;
            put_u64(buf, ph_off, 4096); // p_memsz = 4096
            ph_off += 8;
            put_u64(buf, ph_off, 4096); // p_align
            ph_off += 8;
        }

        // ── Symbol table (section header reuse) ────────────────
        // Sym[0]: NULL symbol
        let mut sym_off = symtab_off;
        put_u32(buf, sym_off, 0);  // st_name
        put_u8(buf, sym_off + 4, 0);  // st_info
        put_u8(buf, sym_off + 5, 0);  // st_other
        put_u16(buf, sym_off + 6, 0); // st_shndx
        put_u64(buf, sym_off + 8, 0); // st_value
        put_u64(buf, sym_off + 16, 0);// st_size
        sym_off += 24;

        // Sym[1..1+count]: one per function
        for (i, entry) in entries.iter().enumerate() {
            // st_name = offset into string table
            let name_off = (str_off - strtab_off) as u32;
            put_u32(buf, sym_off, name_off);
            put_u8(buf, sym_off + 4, 0x12); // STB_GLOBAL | STT_FUNC
            put_u8(buf, sym_off + 5, 0);     // st_other
            put_u16(buf, sym_off + 6, 0);    // st_shndx (absolute)
            put_u64(buf, sym_off + 8, slot_vaddr(i)); // st_value = VDSO slot address
            put_u64(buf, sym_off + 16, 0);   // st_size (unknown)
            sym_off += 24;

            // Write the symbol name string
            let name_bytes = entry.name.as_bytes();
            let mut si = 0;
            while si < name_bytes.len() {
                *buf.get_unchecked_mut(str_off) = name_bytes[si];
                str_off += 1;
                si += 1;
            }
            *buf.get_unchecked_mut(str_off) = 0; // null-terminate
            str_off += 1;
        }

        // ── Section headers (minimal, for compatibility) ────────
        // The symtab and strtab sections at e_shoff:
        // SH[0]: symtab
        let mut sh_off = symtab_off; // reuse same area
        put_u32(buf, sh_off, str_off as u32 - strtab_off as u32 + 1); // sh_name offset in strtab
        put_u32(buf, sh_off + 4, 2);  // SHT_SYMTAB
        put_u64(buf, sh_off + 8, 0);  // sh_flags
        put_u64(buf, sh_off + 16, symtab_off as u64); // sh_addr
        put_u64(buf, sh_off + 24, 0); // sh_offset (not used, sh_addr carries it)
        put_u64(buf, sh_off + 32, (1 + count) as u64 * 24); // sh_size
        put_u32(buf, sh_off + 40, 2); // sh_link = index of strtab section
        put_u32(buf, sh_off + 44, 1); // sh_info
        put_u64(buf, sh_off + 48, 24); // sh_entsize
        sh_off += 56;

        // SH[1]: strtab
        put_u32(buf, sh_off, strtab_off as u32); // sh_name? We'll just set to 0 for now
        put_u32(buf, sh_off + 4, 3);  // SHT_STRTAB
        put_u64(buf, sh_off + 8, 0);
        put_u64(buf, sh_off + 16, strtab_off as u64);
        put_u64(buf, sh_off + 24, 0);
        put_u64(buf, sh_off + 32, (str_off - strtab_off) as u64);
        put_u64(buf, sh_off + 40, 0);
        put_u64(buf, sh_off + 48, 0);
    }

    str_off // total bytes written
}

/// Compute the VDSO virtual address for entry `index`.
pub fn slot_vaddr(index: usize) -> u64 {
    VDSO_USER_BASE + VDSO_META_SIZE as u64 + (index as u64) * 4096
}