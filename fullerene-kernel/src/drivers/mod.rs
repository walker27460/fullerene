pub mod fat;
pub mod sd_card;
pub mod usb_storage;
pub mod virtio_gpu;

use nitrogen::{DriverContext, DriverContextError, PageFlags};

/// Per-driver context used by AHCI and NVMe init wrappers.
struct StorageCtx {
    device_id: u16,
}

impl StorageCtx {
    const fn new(device_id: u16) -> Self {
        Self { device_id }
    }
}

impl DriverContext for StorageCtx {
    fn phys_to_virt(&self, phys: u64) -> usize { crate::ctx::phys_to_virt(phys) }
    fn allocate_frame(&self) -> Result<u64, DriverContextError> { crate::ctx::allocate_frame() }
    fn allocate_contiguous_frames(&self, c: usize) -> Result<u64, DriverContextError> { crate::ctx::allocate_contiguous(c) }
    fn map_mmio_region(&self, p: usize, v: usize, s: usize) -> Result<(), DriverContextError> { crate::ctx::map_mmio(p, v, s) }
    fn map_page(&self, v: usize, p: usize, f: PageFlags) -> Result<(), DriverContextError> { crate::ctx::map_page(v, p, f) }
    fn free_frame(&self, p: u64) { crate::ctx::free_frame(p) }
    fn free_contiguous_frames(&self, p: u64, c: usize) { crate::ctx::free_contiguous(p, c) }
    fn dma_map(&self, _: u16, p: u64, s: usize) -> Result<u64, DriverContextError> {
        crate::ctx::iommu_dma_map(self.device_id, p, s)
    }
    fn dma_unmap(&self, i: u64, s: usize) { crate::ctx::iommu_dma_unmap(i, s) }
}

/// Thin kernel wrapper around `nitrogen::storage::ahci`.
pub fn init_ahci() {
    let ctx = StorageCtx::new(0);
    nitrogen::storage::ahci::init(&ctx);
}

/// Thin kernel wrapper around `nitrogen::storage::nvme`.
pub fn init_nvme() {
    let ctx = StorageCtx::new(0);
    nitrogen::storage::nvme::init(&ctx);
}