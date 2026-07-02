//! Shared helpers for implementing [`DriverContext`] in kernel modules.
//!
//! Each driver creates its own context struct with `device_id` and uses
//! these helpers to delegate to global kernel services.

use nitrogen::{DriverContext, DriverContextError, PageFlags};
use nitrogen::iommu;
use petroleum::initializer::FrameAllocator;
use x86_64::structures::paging::PageTableFlags;

pub(crate) fn phys_to_virt(phys: u64) -> usize {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    (phys + off) as usize
}

pub(crate) fn allocate_frame() -> Result<u64, DriverContextError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(DriverContextError::OutOfMemory)?;
    m.allocate_frame().map_err(|_| DriverContextError::OutOfMemory).map(|p| p as u64)
}

pub(crate) fn allocate_contiguous(count: usize) -> Result<u64, DriverContextError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(DriverContextError::OutOfMemory)?;
    m.allocate_contiguous_frames(count).map_err(|_| DriverContextError::OutOfMemory).map(|p| p as u64)
}

pub(crate) fn map_mmio(phys: usize, virt: usize, size: usize) -> Result<(), DriverContextError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(DriverContextError::MmioMappingFailed)?;
    m.map_mmio_region(phys, virt, size).map_err(|_| DriverContextError::MmioMappingFailed)
}

pub(crate) fn map_page(virt: usize, phys: usize, flags: PageFlags) -> Result<(), DriverContextError> {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    let m = mgr.as_mut().ok_or(DriverContextError::MmioMappingFailed)?;

    let mut pte_flags = PageTableFlags::PRESENT;
    if !flags.executable { pte_flags |= PageTableFlags::NO_EXECUTE; }
    if flags.writable { pte_flags |= PageTableFlags::WRITABLE; }
    if flags.write_combining { pte_flags |= PageTableFlags::WRITE_THROUGH; }

    m.safe_map_page(virt, phys, pte_flags).map_err(|_| DriverContextError::MmioMappingFailed)
}

pub(crate) fn free_frame(phys: u64) {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    if let Some(m) = mgr.as_mut() { let _ = m.free_frame(phys as usize); }
}

pub(crate) fn free_contiguous(phys: u64, count: usize) {
    let mut mgr = crate::memory_management::get_memory_manager().lock();
    if let Some(m) = mgr.as_mut() { let _ = m.free_contiguous_frames(phys as usize, count); }
}

// ── Helper DriverContext for IOMMU operations ────────────────────

/// A ZST [`DriverContext`] used internally by the [`iommu_dma_map`]
/// and [`iommu_dma_unmap`] helpers.  All methods delegate to the
/// global-state helpers above.
struct IommuCtx;

impl DriverContext for IommuCtx {
    fn phys_to_virt(&self, phys: u64) -> usize { phys_to_virt(phys) }
    fn allocate_frame(&self) -> Result<u64, DriverContextError> { allocate_frame() }
    fn allocate_contiguous_frames(&self, c: usize) -> Result<u64, DriverContextError> { allocate_contiguous(c) }
    fn map_mmio_region(&self, p: usize, v: usize, s: usize) -> Result<(), DriverContextError> { map_mmio(p, v, s) }
    fn map_page(&self, v: usize, p: usize, f: PageFlags) -> Result<(), DriverContextError> { map_page(v, p, f) }
    fn free_frame(&self, p: u64) { free_frame(p) }
    fn free_contiguous_frames(&self, p: u64, c: usize) { free_contiguous(p, c) }
    fn dma_map(&self, id: u16, p: u64, s: usize) -> Result<u64, DriverContextError> {
        iommu::dma_map_with_ctx(self, id, p, s)
    }
    fn dma_unmap(&self, iova: u64, size: usize) {
        nitrogen::iommu::dma_unmap(self, iova, size);
    }
}

pub(crate) fn iommu_dma_map(device_id: u16, phys: u64, size: usize) -> Result<u64, DriverContextError> {
    iommu::dma_map_with_ctx(&IommuCtx, device_id, phys, size)
}

pub(crate) fn iommu_dma_unmap(iova: u64, size: usize) {
    iommu::dma_unmap(&IommuCtx, iova, size);
}