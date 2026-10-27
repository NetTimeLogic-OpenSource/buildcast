#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::fs::File;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(windows)]
use self::windows as platform;
#[cfg(windows)]
pub use self::windows::is_elevated;
use crate::Error;

/// Target drive as listed to the user.
#[derive(Clone, Debug, PartialEq)]
pub struct DiskInfo {
    pub bus: String,
    /// `\\.\PhysicalDriveN`, `/dev/sdX` or `diskN`
    pub id: String,
    pub name: String,
    pub removable: bool,
    pub size: u64,
}

// volumes stay locked or dismounted for the lifetime of this struct
pub(crate) struct Device {
    pub(crate) file: File,
    pub(crate) sector: u32,
    pub(crate) size: u64,
    #[cfg(windows)]
    _volumes: Vec<File>,
}

// 4096-byte alignment is required by FILE_FLAG_NO_BUFFERING transfers
pub(crate) struct AlignedBuf {
    layout: Layout,
    len: usize,
    ptr: *mut u8,
}

impl DiskInfo {
    pub fn label(&self) -> String {
        format!("{}  [{}]", self.name, human_size(self.size))
    }

    pub fn label_bus(&self) -> String {
        if self.bus.is_empty() {
            self.label()
        } else {
            format!("{} · {}", self.label(), self.bus)
        }
    }

    /// Lists candidate drives. The system disk is never included.
    ///
    /// Without `all`, only removable or USB drives up to `max_bytes` are
    /// returned.
    pub fn list(all: bool, max_bytes: u64) -> Vec<DiskInfo> {
        platform::list(all, max_bytes)
    }
}

impl Device {
    pub(crate) fn eject(&self, info: &DiskInfo) -> Result<(), Error> {
        platform::eject(self, info)
    }

    /// Unmounts or locks the volumes of `info` and opens the raw device.
    pub(crate) fn open(info: &DiskInfo, write: bool) -> Result<Device, Error> {
        platform::open(info, write)
    }

    // makes the verify pass read the medium, not a cache: Linux drops the
    // page cache here, the Windows handle is NO_BUFFERING and macOS rdisk is
    // uncached
    pub(crate) fn prepare_verify(&self) {
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::posix_fadvise(self.file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }

    // sync_all issues FlushFileBuffers or fsync, which flushes the device
    // write cache
    pub(crate) fn sync(&self) {
        let _ = self.file.sync_all();
    }
}

impl AlignedBuf {
    pub(crate) fn new(len: usize) -> AlignedBuf {
        let layout = Layout::from_size_align(len, 4096).expect("bad layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory");
        AlignedBuf { layout, len, ptr }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

unsafe impl Send for AlignedBuf {}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

#[cfg(unix)]
pub fn is_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn human_size(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(4 * 1024 * 1024), "4.0 MiB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(human_size(2 * 1024u64.pow(4)), "2.0 TiB");
        assert_eq!(human_size(3000 * 1024u64.pow(4)), "3000.0 TiB");
    }

    #[test]
    fn disk_label() {
        let d = DiskInfo {
            bus: "USB".into(),
            id: "x".into(),
            name: "SanDisk Ultra".into(),
            removable: true,
            size: 1024 * 1024 * 1024,
        };
        assert_eq!(d.label(), "SanDisk Ultra  [1.0 GiB]");
        assert_eq!(d.label_bus(), "SanDisk Ultra  [1.0 GiB] · USB");
        let no_bus = DiskInfo {
            bus: String::new(),
            ..d
        };
        assert_eq!(no_bus.label_bus(), no_bus.label());
    }

    #[test]
    fn aligned_buf_is_aligned_and_zeroed() {
        let mut b = AlignedBuf::new(8192);
        assert_eq!(b.as_slice().len(), 8192);
        assert_eq!(b.as_mut_slice().as_ptr() as usize % 4096, 0);
        assert!(b.as_slice().iter().all(|&x| x == 0));
        b.as_mut_slice()[8191] = 7;
        assert_eq!(b.as_slice()[8191], 7);
    }
}
