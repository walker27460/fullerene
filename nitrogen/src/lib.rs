#![no_std]
//! # Nitrogen — Bus Interface Layer
//!
//! Nitrogen is a standalone, `no_std` crate providing:
//!
//! * **PCI** — bus scan, device enumeration, BAR decoding, config-space access.
//! * **ACPI** — RSDP discovery, SDT iteration, table-by-signature lookup.
//! * **Port I/O** — x86 port read/write helpers.
//!
//! Higher-level driver APIs (storage, graphics, networking) are defined
//! elsewhere — nitrogen only owns the bus and platform interfaces.

extern crate alloc;
extern crate core;

pub mod acpi;
pub mod driver_api;
pub mod iommu;
pub mod pci;
pub mod port;