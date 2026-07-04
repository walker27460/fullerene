/// VirtIO GPU driver stub — driver was removed along with nitrogen::virtio.
/// Will be re-added via plugin-based driver API (nitrogen::driver_api::DisplayDriver).

pub fn is_available() -> bool {
    false
}

pub fn get_framebuffer() -> Option<&'static mut [u8]> {
    None
}
