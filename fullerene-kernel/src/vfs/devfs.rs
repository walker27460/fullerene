//! DevFs — a virtual filesystem for device access, mounted at `/dev/`.
//!
//! Each device driver registered via `DriverBox` gets a name in the
//! DevFs namespace (e.g. `/dev/nvme0`, `/dev/eth0`).  Userspace opens,
//! reads, and writes to these paths like regular files — the VFS routes
//! the fd through DevFs to the appropriate `StorageDriver`, `NetworkDriver`,
//! etc. trait method.
//!
//! # Registration
//!
//! During PCI probe, the kernel calls `register_driver(name, driver)` for
//! each successfully loaded driver plugin.  The registration is immediate
//! and persistent — devices appear in `/dev/` as soon as they register.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use genome::vfs::{FileDescriptor, FileSystem, InodeType, VNode};
use nitrogen::driver_api::DriverBox;

// ── Device Registry ───────────────────────────────────────────

static DEVICE_REGISTRY: Mutex<BTreeMap<String, DriverBox>> = Mutex::new(BTreeMap::new());

/// Register a driver under a `/dev/` name (e.g. `"nvme0"`, `"hda0"`, `"eth0"`).
pub fn register_driver(name: &str, driver: DriverBox) {
    DEVICE_REGISTRY.lock().insert(name.to_string(), driver);
}

/// Remove a driver by name.
pub fn unregister_driver(name: &str) {
    DEVICE_REGISTRY.lock().remove(name);
}

/// Check if a driver exists.
pub fn driver_exists(name: &str) -> bool {
    DEVICE_REGISTRY.lock().contains_key(name)
}

/// List all registered device names.
pub fn list_devices() -> Vec<String> {
    DEVICE_REGISTRY.lock().keys().cloned().collect()
}

// ── DevFs ─────────────────────────────────────────────────────

/// A `FileSystem` implementation providing `/dev/` access to device drivers.
pub struct DevFs;

impl DevFs {
    pub const fn new() -> Self {
        Self
    }
}

impl FileSystem for DevFs {
    fn open(&mut self, path: &str, _flags: u32) -> Option<FileDescriptor> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            // Opening the root of /dev/ — not meaningful
            return None;
        }
        // Look up the device by name
        let registry = DEVICE_REGISTRY.lock();
        if registry.contains_key(path) {
            // Allocate a unique inode based on a stable hash of the name.
            // This lets us map back from ino → device during read/write.
            let ino = stable_ino(path);
            let fd = next_fd();
            Some(FileDescriptor { fd, ino, offset: 0, flags: 0 })
        } else {
            None
        }
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        let (name, ino) = lookup_fd(fd).ok_or("bad fd")?;
        let registry = DEVICE_REGISTRY.lock();
        let driver = registry.get(&name).ok_or("device removed")?;
        match driver {
            DriverBox::Storage(drv) => {
                let bs = drv.block_size() as usize;
                let offset = ino.offset;
                let lba = offset / bs;
                let count = buf.len().div_ceil(bs).max(1);
                let actual = count.min(64); // 64 blocks max per read
                let read_bytes = actual * bs;
                let mut tmp = alloc::vec![0u8; read_bytes];
                drv.read_blocks(lba as u64, actual, &mut tmp)?;
                let n = buf.len().min(read_bytes);
                buf[..n].copy_from_slice(&tmp[..n]);
                ino.offset += n;
                Ok(n)
            }
            DriverBox::Network(drv) => {
                drv.receive(buf).or(Ok(0))
            }
            DriverBox::Audio(_) => Err("read not supported on audio device"),
            DriverBox::UsbHost(_) => Err("read not supported on USB host controller"),
            DriverBox::Display(_) => Err("read not supported on display device"),
            DriverBox::None => Err("no device"),
        }
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        let (name, ino) = lookup_fd(fd).ok_or("bad fd")?;
        let registry = DEVICE_REGISTRY.lock();
        let driver = registry.get(&name).ok_or("device removed")?;
        match driver {
            DriverBox::Storage(drv) => {
                let bs = drv.block_size() as usize;
                let offset = ino.offset;
                let lba = offset / bs;
                let count = data.len().div_ceil(bs).max(1);
                let actual = count.min(64);
                let write_bytes = actual * bs;
                let mut tmp = alloc::vec![0u8; write_bytes];
                tmp[..data.len().min(write_bytes)].copy_from_slice(&data[..data.len().min(write_bytes)]);
                drv.write_blocks(lba as u64, actual, &tmp)?;
                ino.offset += data.len();
                Ok(data.len())
            }
            DriverBox::Network(drv) => {
                drv.send(data)?;
                Ok(data.len())
            }
            DriverBox::Audio(drv) => {
                drv.play(data)?;
                Ok(data.len())
            }
            DriverBox::UsbHost(_) => Err("write not supported on USB host controller"),
            DriverBox::Display(_) => Err("write not supported on display device"),
            DriverBox::None => Err("no device"),
        }
    }

    fn close(&mut self, fd: u32) -> Result<(), &'static str> {
        close_fd(fd);
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), &'static str> {
        let (_, ino) = lookup_fd(fd).ok_or("bad fd")?;
        ino.offset = pos;
        Ok(())
    }

    fn create(&mut self, _path: &str, _kind: InodeType) -> Option<u64> {
        None // read-only filesystem
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), &'static str> {
        Err("read-only filesystem")
    }

    fn unlink(&mut self, _path: &str) -> Result<(), &'static str> {
        Err("read-only filesystem")
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, &'static str> {
        let path = path.trim_start_matches('/');
        if !path.is_empty() {
            return Err("not a directory");
        }
        let registry = DEVICE_REGISTRY.lock();
        Ok(registry.keys().map(|name| VNode {
            name: name.clone(),
            size: 0,
            is_dir: false,
        }).collect())
    }

    fn exists(&mut self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        path.is_empty() || DEVICE_REGISTRY.lock().contains_key(path)
    }
}

// ── Internal fd tracking ──────────────────────────────────────

struct FdEntry {
    name: String,
    fd: u32,
    offset: usize,
}

static FD_TABLE: Mutex<Vec<FdEntry>> = Mutex::new(Vec::new());
static NEXT_FD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(100);

fn next_fd() -> u32 {
    NEXT_FD.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

fn lookup_fd(fd: u32) -> Option<(String, *mut usize)> {
    let mut table = FD_TABLE.lock();
    for entry in table.iter_mut() {
        if entry.fd == fd {
            return Some((entry.name.clone(), &mut entry.offset as *mut usize));
        }
    }
    None
}

fn close_fd(fd: u32) {
    FD_TABLE.lock().retain(|e| e.fd != fd);
}

/// Stable inode number from a device name (simple hash).
fn stable_ino(name: &str) -> u64 {
    let mut h: u64 = 0;
    for b in name.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u64);
    }
    h | 0x1000_0000_0000_0000 // high bit to avoid collision with MemFs inodes
}