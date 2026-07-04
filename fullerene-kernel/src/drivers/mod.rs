pub mod fat;
// sd_card, usb_storage, virtio_gpu modules were removed along with DriverContext trait
// and storage/virtio/usb modules from nitrogen. They will be re-added via the new
// plugin-based driver API (nitrogen::driver_api).

use nitrogen::pci::PciDevice;

/// Initialize mass storage devices found on PCI bus.
/// Called during kernel init — placeholder until plugins take over.
pub fn init_storage_devices() {
    // Storage initialization will be handled by driver plugins
    // via the PluginEntry/DriverBox API once PCI scanning is wired up.
}