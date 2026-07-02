//! DriverContext — callback trait for memory allocation and MMIO mapping.
//!
//! Nitrogen drivers that need DMA buffers, MMIO BAR mapping, or physical↔virtual
//! address translation receive a `&dyn DriverContext` from the kernel (or any
//! higher-level crate that owns the memory manager and page tables).
//!
//! # Rationale
//!
//! Nitrogen is a pure hardware-mechanism layer and must not depend on
//! `petroleum` or `fullerene-kernel`.  Instead of calling
//! `petroleum::common::memory::physical_to_virtual()` directly, drivers go
//! through this trait so the kernel retains ownership of the allocator and
//! address-space layout.
//!
//! # Example
//!
//! ```ignore
//! // Kernel side provides an implementation of DriverContext:
//! struct MyDriverContext;
//! impl DriverContext for MyDriverContext { … }
//!
//! // Driver side:
//! pub fn init(ctx: &dyn DriverContext, dev: PciDevice) -> Option<Self> {
//!     let virt = ctx.phys_to_virt(bar_phys);
//!     ctx.map_mmio(bar_phys, virt, bar_size)?;
//!     let frame = ctx.allocate_frame()?;
//!     …
//! }
//! ```
use core::fmt;

/// Error type for driver context operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverContextError {
    /// The requested memory allocation could not be satisfied.
    OutOfMemory,
    /// The MMIO region could not be mapped (e.g. address conflict).
    MmioMappingFailed,
    /// An invalid (null or misaligned) argument was supplied.
    InvalidArgument,
}

impl fmt::Display for DriverContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::MmioMappingFailed => f.write_str("MMIO mapping failed"),
            Self::InvalidArgument => f.write_str("invalid argument"),
        }
    }
}

/// Direction of a DMA transfer.
///
/// Used in [`DmaMapRequest`] to document the intended data flow.  In the
/// current implementation the IOMMU grants read+write access unconditionally,
/// but future revisions may use this hint for cache-coherency management,
/// IOMMU access-control optimisation, or debugging/tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum DmaDirection {
    /// Device reads from memory (host-to-device, e.g. TX / descriptor write).
    ToDevice,
    /// Device writes to memory (device-to-host, e.g. RX / status read).
    FromDevice,
    /// Bidirectional (both read and write).
    Bidirectional,
}

/// A structured DMA-map request that a driver submits to the kernel.
///
/// This combines the allocation and IOMMU-mapping steps into a single
/// operation so drivers don't have to repeat the
/// `allocate_contiguous_frames → zero → phys_to_virt → dma_map` pattern.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DmaMapRequest {
    /// PCI BDF encoded as `((bus as u16) << 8) | (device << 3) | function`.
    pub device_id: u16,
    /// Size of the desired DMA buffer in bytes (rounded up to page boundary).
    pub size: usize,
    /// Optional pre-allocated physical address.  When `Some`, the kernel
    /// maps the caller-supplied memory instead of allocating new frames.
    /// The physical address must be 4 KiB-aligned.
    pub existing_phys: Option<u64>,
    /// Intended data-flow direction.
    pub direction: DmaDirection,
}

impl DmaMapRequest {
    /// Create a new DMA map request that will allocate contiguous frames.
    pub const fn new(device_id: u16, size: usize) -> Self {
        Self {
            device_id,
            size,
            existing_phys: None,
            direction: DmaDirection::Bidirectional,
        }
    }

    /// Attach a pre-allocated physical buffer (must be 4 KiB-aligned).
    pub const fn with_phys(mut self, phys: u64) -> Self {
        self.existing_phys = Some(phys);
        self
    }

    /// Set the DMA direction hint.
    pub const fn with_direction(mut self, direction: DmaDirection) -> Self {
        self.direction = direction;
        self
    }
}

/// The result of a successful [`DriverContext::dma_map_request`] call.
///
/// Holds the IOVA (what the device uses for DMA), the physical address,
/// the kernel-virtual address, and the allocated size so that
/// [`DriverContext::dma_unmap_mapping`] can free everything cleanly.
///
/// When `owns_frames` is true (the default), [`dma_unmap_mapping`] will
/// free the backing physical frames.  When `owns_frames` is false, the
/// caller is responsible for managing the lifetime of any pre-allocated
/// memory passed via [`DmaMapRequest::existing_phys`].
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DmaMapping {
    /// IOVA — IO virtual address the device uses for DMA.
    pub iova: u64,
    /// Physical address of the buffer.
    pub phys: u64,
    /// Kernel-virtual address of the buffer.
    pub virt: *mut u8,
    /// Requested size in bytes.
    pub size: usize,
    /// Number of 4 KiB frames backing this mapping.
    pub pages: usize,
    /// Whether this mapping owns its physical frames (true when allocated
    /// by `dma_map_request`, false when `existing_phys` was provided).
    pub owns_frames: bool,
}

// SAFETY: DmaMapping is only ever constructed by the kernel's DMA
// infrastructure and the pointer is valid for the lifetime of the mapping.
// The kernel is single-threaded for driver init, and after that the
// mapping is only accessed under the driver's own locking.
unsafe impl Send for DmaMapping {}
unsafe impl Sync for DmaMapping {}

impl DmaMapping {
    /// Access the buffer as a mutable byte slice.
    ///
    /// # Safety
    ///
    /// The caller must ensure no DMA is in-flight to/from this buffer
    /// while the slice reference is live (no aliasing device DMA).
    pub unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.size) }
    }

    /// Access the buffer as an immutable byte slice.
    ///
    /// # Safety
    ///
    /// The caller must ensure no DMA write is in-flight from the device
    /// while the slice reference is live.
    pub unsafe fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt, self.size) }
    }
}

/// Services that a driver needs from the owning kernel / runtime.
///
/// All methods are fallible — drivers must handle allocation or mapping
/// failures gracefully, typically by returning `None` from their `init()`.
pub trait DriverContext: Send + Sync {
    /// Convert a physical address to a kernel-accessible virtual address.
    ///
    /// In a higher-half kernel this is typically `phys + offset`.
    fn phys_to_virt(&self, phys: u64) -> usize;

    /// Allocate a single physical 4 KiB frame.
    ///
    /// Returns the **physical** address of the frame.
    fn allocate_frame(&self) -> Result<u64, DriverContextError>;

    /// Allocate `count` contiguous physical 4 KiB frames.
    ///
    /// Returns the **physical** address of the first frame.
    fn allocate_contiguous_frames(&self, count: usize) -> Result<u64, DriverContextError>;

    /// Map a physical MMIO region into the kernel's virtual address space.
    ///
    /// `phys` and `virt` must be page-aligned.  `size` is in bytes.
    fn map_mmio_region(
        &self,
        phys: usize,
        virt: usize,
        size: usize,
    ) -> Result<(), DriverContextError>;

    /// Map a single page with the given flags.
    ///
    /// Used for framebuffer mapping (write-combining, etc.).
    fn map_page(
        &self,
        virt: usize,
        phys: usize,
        flags: PageFlags,
    ) -> Result<(), DriverContextError>;

    /// Free a single physical 4 KiB frame previously returned by
    /// [`allocate_frame`](Self::allocate_frame).
    ///
    /// `phys` must be the exact physical address returned by
    /// `allocate_frame`.  Behaviour is undefined if `phys` was not
    /// allocated through this trait or has already been freed.
    fn free_frame(&self, phys: u64);

    /// Free `count` contiguous physical 4 KiB frames previously returned by
    /// [`allocate_contiguous_frames`](Self::allocate_contiguous_frames).
    ///
    /// `phys` must be the exact physical address returned by
    /// `allocate_contiguous_frames`.  Behaviour is undefined if the region
    /// was not allocated through this trait or has already been freed.
    fn free_contiguous_frames(&self, phys: u64, count: usize);

    /// Map a non-empty, page-aligned, physically-contiguous DMA buffer for device access.
    ///
    /// `phys` must be 4 KiB-aligned and `size` must be non-zero. Implementations
    /// may round `size` up to a whole number of pages.
    /// `device_id` is the PCI BDF encoded as `((bus as u16) << 8) | (device << 3) | function`.
    /// Returns an IOVA (IO Virtual Address) that the device can use for DMA.
    /// If no IOMMU is available, returns the physical address unchanged
    /// (identity mapping).
    fn dma_map(&self, device_id: u16, phys: u64, size: usize) -> Result<u64, DriverContextError>;

    /// Unmap a previously‑mapped DMA buffer.
    ///
    /// `iova` must be the value returned by a prior `dma_map` call, and
    /// `size` must match.  Behaviour is undefined otherwise.
    fn dma_unmap(&self, iova: u64, size: usize);

    /// Allocate and map a DMA buffer in a single operation.
    ///
    /// This is the preferred API for most drivers.  It:
    ///
    /// 1. Allocates contiguous physical frames (or uses
    ///    [`DmaMapRequest::existing_phys`] if provided).
    /// 2. Zeroes the allocated memory.
    /// 3. Maps it through the IOMMU via [`dma_map`](Self::dma_map).
    ///
    /// Returns a [`DmaMapping`] containing the IOVA, physical address,
    /// kernel-virtual address, and size.  The default implementation
    /// delegates to `allocate_contiguous_frames`, `phys_to_virt`,
    /// `dma_map`, and `free_contiguous_frames` on error.
    fn dma_map_request(&self, request: &DmaMapRequest) -> Result<DmaMapping, DriverContextError> {
        let pages = (request.size + 4095) / 4096;
        let size = pages * 4096;

        // If the caller supplied a physical buffer, use it directly;
        // otherwise allocate contiguous frames.
        let (phys, virt): (u64, *mut u8) = if let Some(existing) = request.existing_phys {
            if existing & 0xFFF != 0 {
                return Err(DriverContextError::InvalidArgument);
            }
            let virt = self.phys_to_virt(existing) as *mut u8;
            (existing, virt)
        } else {
            let p = self.allocate_contiguous_frames(pages)?;
            let v = self.phys_to_virt(p) as *mut u8;
            // Zero the allocated memory
            unsafe {
                core::ptr::write_bytes(v, 0, size);
            }
            (p, v)
        };

        // Map through IOMMU
        let iova = self.dma_map(request.device_id, phys, request.size)?;

        let owns_frames = request.existing_phys.is_none();

        Ok(DmaMapping {
            iova,
            phys,
            virt,
            size: request.size,
            pages,
            owns_frames,
        })
    }

    /// Unmap and free a [`DmaMapping`] previously returned by
    /// [`dma_map_request`](Self::dma_map_request).
    ///
    /// Unmaps the IOVA via [`dma_unmap`](Self::dma_unmap) and frees
    /// the physical frames if the mapping owns them (i.e. when they
    /// were allocated by [`dma_map_request`](Self::dma_map_request)
    /// and the request did not supply `existing_phys`).
    fn dma_unmap_mapping(&self, mapping: &DmaMapping) {
        self.dma_unmap(mapping.iova, mapping.size);

        // Only free frames that were allocated by dma_map_request.
        // Caller-supplied memory (existing_phys) is not owned by us.
        if mapping.owns_frames {
            self.free_contiguous_frames(mapping.phys, mapping.pages);
        }
    }
}

/// Simplified page-table flags for driver mapping requests.
///
/// Drivers don't need to know the exact x86 page-table bit layout;
/// they specify semantics through this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    /// Page is writable.
    pub writable: bool,
    /// Page uses write-combining caching (WC) instead of write-back.
    pub write_combining: bool,
    /// Page is executable.
    pub executable: bool,
}

impl PageFlags {
    /// Standard uncacheable MMIO.
    pub const MMIO: Self = Self {
        writable: true,
        write_combining: false,
        executable: false,
    };

    /// Write-combining framebuffer.
    pub const FRAMEBUFFER_WC: Self = Self {
        writable: true,
        write_combining: true,
        executable: false,
    };
}
