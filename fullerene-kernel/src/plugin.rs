//! Plugin registry — tracks registered driver plugins.
//!
//! Each driver plugin registers at initialisation time.  The registry
//! provides a simple iteration interface for kernel subsystems.

use alloc::vec::Vec;
use spin::Mutex;

/// A registered driver plugin entry.
pub struct PluginEntry {
    pub name: &'static str,
}

/// A registry of plugin entries.
pub struct PluginRegistry {
    entries: Vec<PluginEntry>,
}

impl PluginRegistry {
    /// Register a plugin by name.
    pub fn register(name: &'static str) {
        REGISTRY.lock().entries.push(PluginEntry { name });
    }

    /// Iterate over all registered plugins.
    pub fn iter() -> alloc::vec::IntoIter<PluginEntry> {
        REGISTRY.lock().entries.clone().into_iter()
    }

    /// Number of registered plugins.
    pub fn count() -> usize {
        REGISTRY.lock().entries.len()
    }
}

static REGISTRY: Mutex<PluginRegistry> = Mutex::new(PluginRegistry {
    entries: Vec::new(),
});