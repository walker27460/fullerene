//! VT-d IOMMU engine — DMA remapping for PCI devices.
//!
//! # Memory callbacks
//!
//! The IOMMU needs to allocate and map physical pages for its internal
//! page tables.  Instead of depending on a [`DriverContext`]-style trait,
//! the kernel installs raw function pointers via [`set_mem_callbacks`]
//! before calling [`init`].
//!
//! # Usage
//!
//! ```ignore
//! iommu::set_mem_callbacks(iommu::MemCallbacks {
//!     alloc_frame: || { /* allocate a 4KiB frame, return phys addr */ },
//!     free_frame:  |phys| { /* free a frame */ },
//!     phys_to_virt: |phys| { phys + PHYS_OFFSET as usize },
//! });
//! iommu::init(rsdp_phys);
//! ```

pub mod table;
pub mod vtd;

use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use crate::acpi;
use crate::pci::PciScanner;
use table::{IommuPageTable, IommuRootTable};
use vtd::VtdRegisters;

// ── Memory callbacks ─────────────────────────────────────────────

/// Raw callbacks the IOMMU uses to manage physical pages for its
/// internal page tables.  The kernel must install these via
/// [`set_mem_callbacks`] before calling [`init`].
#[derive(Clone, Copy)]
pub struct MemCallbacks {
    pub alloc_frame: fn() -> Option<u64>,
    pub free_frame: fn(u64),
    pub phys_to_virt: fn(u64) -> usize,
}

static MEM: Mutex<Option<MemCallbacks>> = Mutex::new(None);

/// Install the memory callbacks the IOMMU needs for page-table
/// allocation.  Must be called once during kernel init, before
/// [`init`].
pub fn set_mem_callbacks(cbs: MemCallbacks) {
    *MEM.lock() = Some(cbs);
}

fn with_mem<F, R>(f: F) -> Result<R, ()>
where
    F: FnOnce(&MemCallbacks) -> R,
{
    let guard = MEM.lock();
    let cbs = guard.as_ref().ok_or(())?;
    Ok(f(cbs))
}

// ── IOVA Allocator ──────────────────────────────────────────────────

struct IovaInterval {
    start: u64,
    end: u64,
}

struct IovaAllocator {
    base: u64,
    max: u64,
    free: Vec<IovaInterval>,
}

impl IovaAllocator {
    fn new(iova_bits: u8) -> Self {
        let start: u64 = 1 << 12;          // skip page 0
        let max_addr: u64 = (1u64 << iova_bits) - 1;
        Self {
            base: start,
            max: max_addr,
            free: alloc::vec![IovaInterval { start, end: max_addr }],
        }
    }

    fn alloc(&mut self, size: usize) -> Option<u64> {
        let aligned = (size + 4095) & !4095;
        if aligned == 0 {
            return None;
        }
        let need = aligned as u64;
        let mut best: Option<(usize, u64)> = None;
        for (i, iv) in self.free.iter().enumerate() {
            let avail = iv.end - iv.start + 1;
            if avail >= need {
                let candidate = iv.start;
                let fits = match best {
                    Some((_, addr)) => candidate < addr,
                    None => true,
                };
                if fits {
                    best = Some((i, candidate));
                }
            }
        }
        let (idx, addr) = best?;
        let iv = &mut self.free[idx];
        iv.start += need;
        if iv.start > iv.end {
            self.free.remove(idx);
        }
        Some(addr)
    }

    fn free(&mut self, addr: u64, size: usize) {
        let aligned = (size + 4095) & !4095;
        if aligned == 0 {
            return;
        }
        let start = addr;
        let end = addr + aligned as u64 - 1;
        let mut new_free: Vec<IovaInterval> = Vec::new();
        let mut inserted = false;
        for iv in self.free.iter() {
            if !inserted && end < iv.start {
                new_free.push(IovaInterval { start, end });
                new_free.push(IovaInterval { start: iv.start, end: iv.end });
                inserted = true;
            } else if !inserted && start <= iv.end && end >= iv.start {
                let merged_start = start.min(iv.start);
                let merged_end = end.max(iv.end);
                new_free.push(IovaInterval { start: merged_start, end: merged_end });
                inserted = true;
            } else if inserted && start <= iv.start && end >= iv.start {
                if let Some(last) = new_free.last_mut() {
                    last.end = last.end.max(iv.end);
                }
            } else {
                new_free.push(IovaInterval { start: iv.start, end: iv.end });
            }
        }
        if !inserted {
            new_free.push(IovaInterval { start, end });
        }
        self.free = new_free;
    }
}

// ── IommuEngine ─────────────────────────────────────────────────

struct IommuEngine {
    registers: VtdRegisters,
    root_table: IommuRootTable,
    page_table: IommuPageTable,
    iova: IovaAllocator,
}

// SAFETY: IommuEngine is only accessed under the GLOBAL_IOMMU mutex.
unsafe impl Send for IommuEngine {}

impl IommuEngine {
    fn new(regs: VtdRegisters, iova_bits: u8) -> Result<Self, ()> {
        let mut cbs = MEM.lock();
        let ctx = cbs.as_ref().ok_or(())?;
        let root_table = IommuRootTable::new(ctx)?;
        let page_table = IommuPageTable::new(ctx, 0)?;
        let iova = IovaAllocator::new(iova_bits);
        Ok(Self { registers: regs, root_table, page_table, iova })
    }

    fn set_device_context(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Result<(), ()> {
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(())?;
        let entry = self.root_table.get_context_entry(ctx, bus, device, function)?;
        let aw_bits = table::CTX_AW_3LEVEL;
        *entry = table::ContextEntry::new_host(self.page_table.root_phys(), aw_bits);
        Ok(())
    }

    fn setup_default_blocked_all(&mut self) -> Result<(), ()> {
        let scanner = PciScanner::new();
        if scanner.get_devices().is_empty() {
            log::warn!("IOMMU: no PCI devices found, skipping default-blocked setup");
            return Ok(());
        }
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(())?;
        for dev_info in scanner.get_devices() {
            let entry = self.root_table.get_context_entry(
                ctx,
                dev_info.bus,
                dev_info.device,
                dev_info.function,
            )?;
            *entry = table::ContextEntry::new_blocked();
        }
        Ok(())
    }

    fn dma_map(&mut self, device_id: u16, phys: u64, size: usize) -> Result<u64, ()> {
        let bus = (device_id >> 8) as u8;
        let device = ((device_id >> 3) & 0x1F) as u8;
        let function = (device_id & 0x7) as u8;

        // Set up context entry for this device if not already done.
        // Idempotent once configured.
        self.set_device_context(bus, device, function)?;

        let iova = self.iova.alloc(size).ok_or(())?;
        let pages = (size + 4095) / 4096;
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(())?;

        for i in 0..pages {
            let iova_page = iova + (i as u64) * 4096;
            let phys_page = phys + (i as u64) * 4096;
            if self.page_table.map_page(ctx, iova_page, phys_page).is_err() {
                // Rollback
                for j in 0..i {
                    self.page_table.unmap_page(ctx, iova + (j as u64) * 4096);
                }
                self.iova.free(iova, size);
                return Err(());
            }
        }

        // Flush caches
        self.registers.context_cache_invalidate_domain(self.page_table.domain_id());
        self.registers.iotlb_domain_invalidate(self.page_table.domain_id());
        Ok(iova)
    }

    fn dma_unmap(&mut self, iova: u64, size: usize) {
        let mut cbs = MEM.lock();
        let ctx = cbs.as_mut().ok_or(()).unwrap();
        let pages = (size + 4095) / 4096;
        for i in 0..pages {
            let iova_page = iova + (i as u64) * 4096;
            self.page_table.unmap_page(ctx, iova_page);
        }
        self.iova.free(iova, size);
        self.registers.iotlb_domain_invalidate(self.page_table.domain_id());
    }
}

// ── Global state ─────────────────────────────────────────────────

static GLOBAL_IOMMU: Mutex<Option<IommuEngine>> = Mutex::new(None);
static IOMMU_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialise the IOMMU from the ACPI DMAR table.
///
/// `rsdp_phys` is the physical address of the RSDP, obtained from
/// [`acpi::find_rsdp`] or passed by the bootloader.
///
/// The kernel must have installed memory callbacks via
/// [`set_mem_callbacks`] before calling this function.
pub fn init(rsdp_phys: u64) -> Result<(), &'static str> {
    if IOMMU_INITIALIZED.load(core::sync::atomic::Ordering::Relaxed) {
        log::info!("IOMMU: already initialized, re-init skipped");
        return Ok(());
    }

    let dmar = acpi::dmar::parse_dmar(rsdp_phys).ok_or("failed to parse DMAR table")?;

    if dmar.drhd_units.is_empty() {
        return Err("IOMMU: no DRHD units found");
    }

    let drhd = &dmar.drhd_units[0];
    let mmio_base = drhd.phys_base;
    let iova_bits = dmar.host_address_width;

    log::info!(
        "IOMMU: mmio_base={:#x} host_addr_width={} drhd_units={}",
        mmio_base,
        iova_bits,
        dmar.drhd_units.len(),
    );

    let regs = VtdRegisters::new(mmio_base);
    let ver = regs.version();
    let cap = regs.cap();
    let ecap = regs.ecap();
    log::info!(
        "IOMMU: version={:#x} cap={:#x} ecap={:#x} domains={} mgaw={} sagaw={}",
        ver, cap, ecap,
        vtd::cap_num_domains(cap),
        vtd::cap_mgaw(cap),
        vtd::cap_sagaw(cap),
    );

    // Block all PCI devices from DMA by default before enabling translation
    let mut engine = IommuEngine::new(regs, iova_bits)
        .map_err(|_| "IOMMU engine creation failed")?;
    engine.setup_default_blocked_all()
        .map_err(|_| "IOMMU default-blocked setup failed")?;

    // Check if hardware is already enabled by firmware
    if engine.registers.gsts() & vtd::GSTS_TES != 0 {
        log::info!("IOMMU: already enabled by firmware");
    }

    // Set root table pointer
    engine.registers.write_buffer_flush();
    engine.registers.set_rtaddr(engine.root_table.root_phys());
    engine.registers.wait_for_root_table_ptr();

    // Enable translation
    let cmd = engine.registers.gcmd();
    engine.registers.set_gcmd(cmd | vtd::GCMD_TE);
    engine.registers.wait_for_translation_enable();

    *GLOBAL_IOMMU.lock() = Some(engine);
    IOMMU_INITIALIZED.store(true, core::sync::atomic::Ordering::Release);
    log::info!("IOMMU: initialized with DMA blocked for all devices by default");
    Ok(())
}

/// Map a physically-contiguous buffer through the IOMMU.
///
/// Returns an IOVA the device can use for DMA.
pub fn dma_map(device_id: u16, phys: u64, size: usize) -> Result<u64, ()> {
    let mut guard = GLOBAL_IOMMU.lock();
    let engine = guard.as_mut().ok_or(())?;
    engine.dma_map(device_id, phys, size)
}

/// Unmap a previously-mapped IOVA.
pub fn dma_unmap(iova: u64, size: usize) {
    let mut guard = GLOBAL_IOMMU.lock();
    if let Some(ref mut engine) = *guard {
        engine.dma_unmap(iova, size);
    }
}