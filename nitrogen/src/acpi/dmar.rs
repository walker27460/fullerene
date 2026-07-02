//! DMA Remapping (DMAR) table parsing.
//!
//! Parses the ACPI DMAR table via raw pointer access to discover
//! VT-d hardware units and their PCI device scopes.

use alloc::vec::Vec;
use crate::acpi;

/// A DMA Remapping Hardware Unit Description (DRHD) entry.
#[derive(Debug, Clone)]
pub struct DmarDrhd {
    pub flags: u8,
    pub segment: u16,
    pub phys_base: u64,
    pub dev_scope_bus: u8,
    pub dev_scope_path: Vec<(u8, u8)>,
}

/// Parsed DMAR table information.
#[derive(Debug, Clone)]
pub struct DmarInfo {
    pub host_address_width: u8,
    pub drhd_units: Vec<DmarDrhd>,
}

/// Parse the DMAR table given the RSDP physical address.
pub fn parse_dmar(rsdp_phys: u64) -> Option<DmarInfo> {
    let bytes = acpi::get_table_bytes(acpi::find_table(rsdp_phys, b"DMAR")?)?;
    let total_len = bytes.len();
    let p = bytes.as_ptr();

    // host_address_width at offset 36
    let host_address_width = unsafe { core::ptr::read_unaligned(p.add(36) as *const u8) };

    let mut drhd_units = Vec::new();
    let mut offset = 48usize; // SdtHeader(36) + DmarTable(12)

    while offset + 4 <= total_len {
        let stype = unsafe { core::ptr::read_unaligned(p.add(offset) as *const u16) };
        let slen = unsafe { core::ptr::read_unaligned(p.add(offset + 2) as *const u16) } as usize;
        if slen < 4 || offset + slen > total_len {
            return None;
        }
        if stype == 0 && slen >= 20 {
            // DRHD: flags(1) + reserved(1) + segment(2) + phys_base(8) + device scope
            let flags = unsafe { core::ptr::read_unaligned(p.add(offset + 4) as *const u8) };
            let segment = unsafe { core::ptr::read_unaligned(p.add(offset + 6) as *const u16) };
            let phys_base = unsafe { core::ptr::read_unaligned(p.add(offset + 8) as *const u64) };

            let mut bus = 0u8;
            let mut path = Vec::new();
            let scope_off = offset + 16;

            if scope_off + 6 <= offset + slen {
                let scope_len = unsafe { core::ptr::read_unaligned(p.add(scope_off + 1) as *const u8) } as usize;
                bus = unsafe { core::ptr::read_unaligned(p.add(scope_off + 5) as *const u8) };
                let mut po = scope_off + 6;
                while po + 2 <= scope_off + scope_len {
                    let dev = unsafe { core::ptr::read_unaligned(p.add(po) as *const u8) };
                    let func = unsafe { core::ptr::read_unaligned(p.add(po + 1) as *const u8) };
                    path.push((dev, func));
                    po += 2;
                }
            }

            drhd_units.push(DmarDrhd { flags, segment, phys_base, dev_scope_bus: bus, dev_scope_path: path });
        }
        offset += slen;
    }

    Some(DmarInfo { host_address_width, drhd_units })
}