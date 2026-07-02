//! C‑compatible DMA map request API for kernel drivers.
//!
//! Drivers written in C, C++, or any language with C FFI can use
//! these `extern "C"` functions directly.  The request and result
//! types are `nitrogen`'s own [`DmaMapRequest`] and [`DmaMapping`]
//! — both are `#[repr(C)]` so there is no ABI shim necessary.
//!
//! All functions operate on the kernel's global [`DriverContext`] —
//! there is no per-driver context to manage.
//!
//! # Error handling
//!
//! Functions return `0` on success and `-1` on error.  The error is
//! best-effort logged; no `errno` mechanism is provided yet.
//!
//! # C header
//!
//! ```c
//! #include <stdint.h>
//!
//! /* DmaDirection */
//! #define DMA_TO_DEVICE      0
//! #define DMA_FROM_DEVICE    1
//! #define DMA_BIDIRECTIONAL  2
//!
//! typedef struct {
//!     uint16_t device_id;       /* PCI BDF */
//!     uint64_t size;
//!     uint64_t existing_phys;   /* 0 = allocate new frames */
//!     uint32_t direction;       /* 0=ToDevice, 1=FromDevice, 2=Bidirectional */
//! } DmaMapRequest;
//!
//! typedef struct {
//!     uint64_t iova;
//!     uint64_t phys;
//!     uint64_t size;
//!     uint32_t pages;
//!     uint32_t owns_frames;     /* 0 = no, 1 = yes */
//! } DmaMapping;
//!
//! /* Returns 0 on success, -1 on error */
//! int ffi_dma_map_request(const DmaMapRequest* request, DmaMapping* out_mapping);
//! int ffi_dma_unmap_mapping(const DmaMapping* mapping);
//! ```
//!
//! [`DriverContext`]: nitrogen::DriverContext

use core::ptr;

use nitrogen::{DmaMapRequest, DmaMapping, DriverContext};

// ── extern "C" API ────────────────────────────────────────────────

/// Driver entry point.
///
/// Every driver module exports this symbol.  The kernel calls it during
/// boot to let the driver initialise its hardware and register its
/// [`DriverContext`] via [`ffi_register_driver_context`].
///
/// # Safety
///
/// This function is called once at boot.  The implementor must not
/// assume any particular execution context (interrupts may or may not
/// be enabled).
///
/// # C definition
///
/// ```c
/// void kernel_main(void);
/// ```
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kernel_main() {
    // Default implementation is a no-op.  Driver plugins override this
    // symbol at link time.
}

/// Register a [`DriverContext`] from a driver module.
///
/// Drivers call this from their [`kernel_main`] entry point.
///
/// # Safety
///
/// `ctx` must point to a `'static` context that outlives the call.
/// Passing a dangling pointer is undefined behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_register_driver_context(ctx: &'static dyn DriverContext) {
    crate::plugin::PluginRegistry::register(ctx);
    if let Some(name) = core::any::type_name_of_val(ctx).strip_prefix("fullerene_kernel::") {
        log::info!("driver: registered context ({})", name);
    }
}

/// Map a DMA buffer with a single C call.
///
/// * `request` — non-null pointer to a filled-in [`DmaMapRequest`].
/// * `out_mapping` — non-null pointer to receive the resulting
///   [`DmaMapping`].
///
/// Returns `0` on success, `-1` on error (bad pointer or allocation
/// failure).
///
/// # Safety
///
/// Both pointers must be valid, aligned, non-overlapping, and
/// non-null.  The kernel's page-table and IOMMU are modified.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_dma_map_request(
    request: *const DmaMapRequest,
    out_mapping: *mut DmaMapping,
) -> i32 {
    let req = match unsafe { request.as_ref() } {
        Some(r) => r,
        None => return -1,
    };
    let out = match unsafe { out_mapping.as_mut() } {
        Some(o) => o,
        None => return -1,
    };

    struct _Ctx;
impl nitrogen::DriverContext for _Ctx {
    fn phys_to_virt(&self, p: u64) -> usize { crate::ctx::phys_to_virt(p) }
    fn allocate_frame(&self) -> Result<u64, nitrogen::DriverContextError> { crate::ctx::allocate_frame() }
    fn allocate_contiguous_frames(&self, c: usize) -> Result<u64, nitrogen::DriverContextError> { crate::ctx::allocate_contiguous(c) }
    fn map_mmio_region(&self, p: usize, v: usize, s: usize) -> Result<(), nitrogen::DriverContextError> { crate::ctx::map_mmio(p, v, s) }
    fn map_page(&self, v: usize, p: usize, f: nitrogen::PageFlags) -> Result<(), nitrogen::DriverContextError> { crate::ctx::map_page(v, p, f) }
    fn free_frame(&self, p: u64) { crate::ctx::free_frame(p) }
    fn free_contiguous_frames(&self, p: u64, c: usize) { crate::ctx::free_contiguous(p, c) }
    fn dma_map(&self, id: u16, p: u64, s: usize) -> Result<u64, nitrogen::DriverContextError> { crate::ctx::iommu_dma_map(id, p, s) }
    fn dma_unmap(&self, i: u64, s: usize) { crate::ctx::iommu_dma_unmap(i, s) }
}
    let ctx = _Ctx;

    match ctx.dma_map_request(req) {
        Ok(mapping) => {
            *out = mapping;
            0
        }
        Err(e) => {
            log::error!("ffi_dma_map_request failed: {:?}", e);
            -1
        }
    }
}

/// Unmap and free a DMA mapping previously returned by
/// [`ffi_dma_map_request`].
///
/// * `mapping` — non-null pointer to the mapping to tear down.
///
/// Returns `0` on success, `-1` if `mapping` is null.
///
/// # Safety
///
/// `mapping` must point to a valid mapping obtained from
/// `ffi_dma_map_request`.  The mapping must not be in use by any
/// device DMA at the time of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ffi_dma_unmap_mapping(mapping: *const DmaMapping) -> i32 {
    let m = match unsafe { mapping.as_ref() } {
        Some(m) => m,
        None => return -1,
    };

    // Reconstruct a DmaMapping without virt — the C caller never had
    // access to it, and dma_unmap_mapping only needs iova/phys/size/pages.
    let m = DmaMapping {
        iova: m.iova,
        phys: m.phys,
        virt: ptr::null_mut(),
        size: m.size,
        pages: m.pages,
        owns_frames: m.owns_frames,
    };

    struct _Ctx;
impl nitrogen::DriverContext for _Ctx {
    fn phys_to_virt(&self, p: u64) -> usize { crate::ctx::phys_to_virt(p) }
    fn allocate_frame(&self) -> Result<u64, nitrogen::DriverContextError> { crate::ctx::allocate_frame() }
    fn allocate_contiguous_frames(&self, c: usize) -> Result<u64, nitrogen::DriverContextError> { crate::ctx::allocate_contiguous(c) }
    fn map_mmio_region(&self, p: usize, v: usize, s: usize) -> Result<(), nitrogen::DriverContextError> { crate::ctx::map_mmio(p, v, s) }
    fn map_page(&self, v: usize, p: usize, f: nitrogen::PageFlags) -> Result<(), nitrogen::DriverContextError> { crate::ctx::map_page(v, p, f) }
    fn free_frame(&self, p: u64) { crate::ctx::free_frame(p) }
    fn free_contiguous_frames(&self, p: u64, c: usize) { crate::ctx::free_contiguous(p, c) }
    fn dma_map(&self, id: u16, p: u64, s: usize) -> Result<u64, nitrogen::DriverContextError> { crate::ctx::iommu_dma_map(id, p, s) }
    fn dma_unmap(&self, i: u64, s: usize) { crate::ctx::iommu_dma_unmap(i, s) }
}
    let ctx = _Ctx;
    ctx.dma_unmap_mapping(&m);
    0
}