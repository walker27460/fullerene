//! ACPI table discovery and parsing.
//!
//! Provides RSDP discovery (EBDA / BIOS ROM scan), root-system-descriptor
//! table iteration (XSDT / RSDT), and generic table-by-signature lookup.
//!
//! All ACPI structures are accessed via `#[repr(C, packed)]` types cast
//! directly from the physical-memory mapping — no manual offset arithmetic.
//!
//! # Usage
//!
//! ```ignore
//! acpi::set_phys_to_virt_offset(PHYS_OFFSET);
//! let rsdp = acpi::find_rsdp().expect("ACPI not available");
//! let dmar = acpi::find_table(rsdp, b"DMAR").expect("DMAR not found");
//! ```

pub mod dmar;

use core::sync::atomic::AtomicU64;

pub use dmar::parse_dmar;

// ── ACPI table structures (packed) ───────────────────────────────

/// RSDP — Root System Description Pointer (ACPI v1 / v2+).
#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct Rsdp {
    signature: [u8; 8],       // "RSD PTR "
    checksum: u8,             // first 20 bytes checksum
    oem_id: [u8; 6],
    revision: u8,             // 0=v1, 2=v2+
    rsdt_address: u32,        // RSDT physical address (v1)
    // v2+ fields:
    length: u32,              // total RSDP length (36 for v2+)
    xsdt_address: u64,        // XSDT physical address (v2+)
    extended_checksum: u8,
    _reserved: [u8; 3],
}

/// Common ACPI table header — every SDT starts with this.
#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

/// XSDT (or RSDT) entry.  XSDT uses `u64` entries, RSDT uses `u32`.
/// We read entries through raw pointer casts and branch on entry size.

// ── Physical-memory offset ───────────────────────────────────────

static PHYS_TO_VIRT: AtomicU64 = AtomicU64::new(0);

/// Set the physical-memory offset used for ACPI table access.
///
/// Must be called once during kernel init so that `phys_to_virt` works.
pub fn set_phys_to_virt_offset(offset: u64) {
    PHYS_TO_VIRT.store(offset, core::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn phys_to_virt<T>(phys: u64) -> *const T {
    let offset = PHYS_TO_VIRT.load(core::sync::atomic::Ordering::Relaxed);
    phys.checked_add(offset).map(|v| v as *const T).unwrap_or(core::ptr::null())
}

fn phys_to_virt_mut<T>(phys: u64) -> *mut T {
    let offset = PHYS_TO_VIRT.load(core::sync::atomic::Ordering::Relaxed);
    phys.checked_add(offset).map(|v| v as *mut T).unwrap_or(core::ptr::null_mut())
}

fn checksum(data: &[u8]) -> bool {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

// ── RSDP discovery ───────────────────────────────────────────────

const EBDA_SEG_PTR: u64 = 0x0000_0000_0000_040E;
const EBDA_SEG_LEN: u64 = 0x0000_0000_0000_0400;
const BIOS_ROM_START: u64 = 0x0000_0000_000E_0000;
const BIOS_ROM_END: u64 = 0x0000_0000_000F_FFFF;

fn find_rsdp_at(phys: u64) -> Option<u64> {
    use core::ptr::addr_of;
    let ptr = phys_to_virt::<Rsdp>(phys);
    if ptr.is_null() {
        return None;
    }
    let sig = unsafe { core::ptr::read_unaligned(addr_of!((*ptr).signature)) };
    if &sig != b"RSD PTR " {
        return None;
    }
    let rev = unsafe { core::ptr::read_unaligned(addr_of!((*ptr).revision)) };
    let cksum_len = if rev >= 2 { 36 } else { 20 };
    let data = unsafe { core::slice::from_raw_parts(ptr as *const u8, cksum_len) };
    if checksum(data) { Some(phys) } else { None }
}

fn find_rsdp_in_range(start: u64, len: u64) -> Option<u64> {
    let mut addr = start;
    while addr < start + len {
        if find_rsdp_at(addr).is_some() {
            return Some(addr);
        }
        addr += 16;
    }
    None
}

/// Scan the EBDA and BIOS ROM for the RSDP table.
pub fn find_rsdp() -> Option<u64> {
    let ebda_ptr = unsafe {
        let p = phys_to_virt::<u16>(EBDA_SEG_PTR);
        if p.is_null() { 0 } else { *p as u64 * 16 }
    };
    if ebda_ptr > 0 {
        if let Some(addr) = find_rsdp_in_range(ebda_ptr, EBDA_SEG_LEN) {
            return Some(addr);
        }
    }
    find_rsdp_in_range(BIOS_ROM_START, BIOS_ROM_END - BIOS_ROM_START + 1)
}

/// Validate an RSDP at a known physical address.
pub fn find_rsdp_from_addr(addr: u64) -> bool {
    find_rsdp_at(addr).is_some()
}

// ── SDT (System Description Table) iteration ─────────────────────

/// Find an ACPI table by its 4-byte signature.
///
/// `rsdp_phys` is the physical address of the RSDP (from [`find_rsdp`]).
/// `signature` is a 4-byte table signature such as `b"DMAR"`, `b"APIC"`,
/// `b"HPET"`, etc.
///
/// Returns the physical address of the matching table, or `None`.
pub fn find_table(rsdp_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    use core::ptr::addr_of;

    // Validate and read RSDP.
    if find_rsdp_at(rsdp_phys).is_none() {
        return None;
    }
    let rsdp_ptr = phys_to_virt::<Rsdp>(rsdp_phys);
    if rsdp_ptr.is_null() {
        return None;
    }

    // Determine the primary SDT (XSDT on v2+, RSDT on v1).
    let rev = unsafe { core::ptr::read_unaligned(addr_of!((*rsdp_ptr).revision)) };
    let (sdt_phys, entry_size): (u64, usize) = if rev >= 2 {
        let xsdt = unsafe { core::ptr::read_unaligned(addr_of!((*rsdp_ptr).xsdt_address)) };
        if xsdt != 0 { (xsdt, 8) } else { return None; }
    } else {
        let rsdt = unsafe { core::ptr::read_unaligned(addr_of!((*rsdp_ptr).rsdt_address)) } as u64;
        if rsdt != 0 { (rsdt, 4) } else { return None; }
    };

    let sdt_ptr = phys_to_virt::<SdtHeader>(sdt_phys);
    if sdt_ptr.is_null() {
        return None;
    }
    let length = unsafe { core::ptr::read_unaligned(addr_of!((*sdt_ptr).length)) };

    if length > 128 * 1024 || length < (36 + entry_size) as u32 {
        return None;
    }

    let entry_count = (length as usize - 36) / entry_size;
    let entries_virt = sdt_ptr as usize + 36;

    for i in 0..entry_count {
        let entry_phys = if entry_size == 8 {
            unsafe { *(entries_virt as *const u64).add(i) }
        } else {
            unsafe { *(entries_virt as *const u32).add(i) as u64 }
        };
        if entry_phys == 0 {
            continue;
        }
        let tbl = phys_to_virt::<[u8; 4]>(entry_phys);
        if tbl.is_null() {
            continue;
        }
        let sig = unsafe { core::ptr::read_unaligned(tbl) };
        if &sig == signature {
            return Some(entry_phys);
        }
    }
    None
}

/// Get a pointer to a table's raw bytes for DMAR and other parsers.
pub fn get_table_bytes(phys: u64) -> Option<&'static [u8]> {
    let sdt = phys_to_virt::<SdtHeader>(phys);
    if sdt.is_null() {
        return None;
    }
    let header = unsafe { &*sdt };
    if header.length > 128 * 1024 || header.length < 36 {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(sdt as *const u8, header.length as usize) })
}