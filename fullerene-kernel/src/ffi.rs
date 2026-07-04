//! C‑compatible DMA map request API for kernel drivers.
//!
//! Drivers written in C, C++, or any language with C FFI can use
//! these `extern "C"` functions directly.  The request and result
//! types are defined below — all are `#[repr(C)]` so there is no
//! ABI shim necessary.
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

use core::ptr;

// ── DMA types (matching the C ABI) ──────────────────────────────

/// Direction of a DMA transfer.
#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum DmaDirection {
    ToDevice = 0,
    FromDevice = 1,
    Bidirectional = 2,
}

/// Parameters for a DMA map request.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DmaMapRequest {
    pub device_id: u16,
    pub size: u64,
    pub existing_phys: u64,
    pub direction: DmaDirection,
}

/// Result of a successful DMA map.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DmaMapping {
    pub iova: u64,
    pub phys: u64,
    pub size: u64,
    pub pages: u32,
    pub owns_frames: u32,
}

// ── extern "C" API ────────────────────────────────────────────

/// Driver entry point.
///
/// Every driver module exports this symbol.  The kernel calls it during
/// boot to let the driver initialise its hardware.
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

    // Allocate or use existing physical frames
    let phys = if req.existing_phys != 0 {
        req.existing_phys
    } else {
        match crate::ctx::allocate_contiguous(
            (req.size as usize + 4095) / 4096,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("ffi_dma_map_request: alloc failed: {:?}", e);
                return -1;
            }
        }
    };

    // Map through IOMMU
    match crate::ctx::iommu_dma_map(req.device_id, phys, req.size as usize) {
        Ok(iova) => {
            let pages = (req.size as usize + 4095) / 4096;
            *out = DmaMapping {
                iova,
                phys,
                size: req.size,
                pages: pages as u32,
                owns_frames: if req.existing_phys != 0 { 0 } else { 1 },
            };
            0
        }
        Err(()) => {
            log::error!("ffi_dma_map_request: iommu_map failed");
            // Free the allocated frames on failure (if we allocated them)
            if req.existing_phys == 0 {
                crate::ctx::free_contiguous(phys, (req.size as usize + 4095) / 4096);
            }
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

    crate::ctx::iommu_dma_unmap(m.iova, m.size as usize);

    // Free the physical frames if the mapping owns them
    if m.owns_frames != 0 {
        crate::ctx::free_contiguous(m.phys, m.pages as usize);
    }

    0
}