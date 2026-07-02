//! Registry of per-device [`DriverContext`] trait objects.
//!
//! The kernel creates a [`KernelContext`] per PCI device (or reuses
//! the shared `&KernelContext` for infrastructure) and registers it
//! here so subsystems can enumerate active driver contexts for power
//! management, device enumeration, and diagnostics.
//!
//! # Example
//!
//! ```ignore
//! // Register a driver context:
//! static NVME_CTX: KernelContext = KernelContext;
//! crate::plugin::PluginRegistry::register(&NVME_CTX);
//!
//! // Iterate all registered contexts:
//! for ctx in PluginRegistry::iter() {
//!     let _: &dyn DriverContext = ctx;
//! }
//! ```
//!
//! [`KernelContext`]: crate::ctx::KernelContext

use alloc::vec::Vec;
use nitrogen::DriverContext;
use spin::Mutex;

/// A registry of [`DriverContext`] objects.
///
/// Each driver plugin registers its context at initialisation time.
/// The registry provides a simple iteration interface for kernel
/// subsystems that need to operate on all active contexts.
pub struct PluginRegistry {
    contexts: Vec<&'static dyn DriverContext>,
}

impl PluginRegistry {
    /// Register a driver context.
    ///
    /// The context must be a `'static` reference — typically a
    /// `static` item in the driver module, or `&KernelContext`.
    pub fn register(ctx: &'static dyn DriverContext) {
        REGISTRY.lock().contexts.push(ctx);
    }

    /// Iterate over all registered driver contexts.
    pub fn iter() -> alloc::vec::IntoIter<&'static dyn DriverContext> {
        REGISTRY.lock().contexts.clone().into_iter()
    }

    /// Number of registered driver contexts.
    pub fn count() -> usize {
        REGISTRY.lock().contexts.len()
    }
}

static REGISTRY: Mutex<PluginRegistry> = Mutex::new(PluginRegistry {
    contexts: Vec::new(),
});