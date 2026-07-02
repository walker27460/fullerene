//! Per-PCI-subclass driver API traits and plugin entry point.
//!
//! Each PCI base class defines a trait that a driver plugin implements.
//! The kernel discovers drivers via a standard [`PluginEntry`] and
//! receives a [`DriverBox`] containing the driver's trait object.
//!
//! # Adding a new subclass
//!
//! 1. Define a new trait (e.g. `MyDriver: Send`).
//! 2. Add a variant to [`DriverBox`].
//! 3. Add a match arm in the kernel's dispatch logic.

use alloc::boxed::Box;
use crate::pci::PciDevice;

// ── Mass storage (PCI class 0x01) ───────────────────────────────

/// Block-device interface for mass-storage controllers
/// (NVMe, AHCI, SATA, IDE, SD/MMC, USB mass storage, etc.).
pub trait StorageDriver: Send {
    /// Initialise the device after PCI enumeration.
    fn init(&mut self) -> Result<(), &'static str>;

    /// Read `count` blocks starting at `lba` into `buf`.
    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), &'static str>;

    /// Write `count` blocks starting at `lba` from `buf`.
    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), &'static str>;

    /// Logical block size in bytes (typically 512 or 4096).
    fn block_size(&self) -> u32;

    /// Total number of addressable blocks.
    fn total_blocks(&self) -> u64;
}

// ── Network (PCI class 0x02) ────────────────────────────────────

/// Network interface controller (Ethernet, Wi-Fi, etc.).
pub trait NetworkDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn send(&self, buf: &[u8]) -> Result<(), &'static str>;
    fn receive(&self, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn mac_address(&self) -> [u8; 6];
}

// ── Display (PCI class 0x03) ────────────────────────────────────

/// Display / GPU controller (VGA-compatible, VirtIO-GPU, etc.).
pub trait DisplayDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn framebuffer(&self) -> &[u8];
    fn resolution(&self) -> (usize, usize);
    fn stride(&self) -> usize;
    fn flush(&self);
}

// ── Multimedia – audio (PCI class 0x04, subclass 0x01) ──────────

pub trait AudioDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn play(&self, buf: &[u8]) -> Result<(), &'static str>;
}

// ── Serial bus – USB (PCI class 0x0C, subclass 0x03) ────────────

pub trait UsbHostDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn poll(&self);
}

// ── DriverBox ───────────────────────────────────────────────────

/// Type-erased return from a plugin entry point.
///
/// The kernel matches the returned variant against the PCI device's
/// (class, subclass) to dispatch the correct driver API.
pub enum DriverBox {
    Storage(Box<dyn StorageDriver>),
    Network(Box<dyn NetworkDriver>),
    Display(Box<dyn DisplayDriver>),
    Audio(Box<dyn AudioDriver>),
    UsbHost(Box<dyn UsbHostDriver>),
    /// The plugin did not handle this device.
    None,
}

// ── Plugin entry point ──────────────────────────────────────────

/// Entry point every PCI-subclass driver plugin exports.
///
/// The kernel enumerates PCI devices and calls each registered entry
/// point with the device.  The driver probes the device and returns
/// a [`DriverBox`] — either the appropriate trait variant or `None`.
///
/// # Example
///
/// ```ignore
/// #[unsafe(no_mangle)]
/// pub fn kernel_main(dev: &PciDevice) -> DriverBox {
///     if dev.class_code != 0x01 || dev.subclass != 0x08 {
///         return DriverBox::None;  // not NVMe
///     }
///     match NvmeController::init(dev) {
///         Ok(ctrl) => DriverBox::Storage(Box::new(ctrl)),
///         Err(_)   => DriverBox::None,
///     }
/// }
/// ```
pub type PluginEntry = fn(device: &PciDevice) -> DriverBox;