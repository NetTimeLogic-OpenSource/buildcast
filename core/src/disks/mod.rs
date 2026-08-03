use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::File;
use std::io;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{eject_disk, list_disks, open_disk};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{eject_disk, list_disks, open_disk};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{eject_disk, is_elevated, list_disks, open_disk};

#[cfg(unix)]
pub fn is_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiskInfo {
    // `\\.\PhysicalDriveN`, `/dev/sdX` or `diskN`
    pub id: String,
    pub name: String,
    pub size: u64,
    pub removable: bool,
    pub bus: String,
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
}

// volumes stay locked/dismounted for the lifetime of this struct
pub struct Device {
    pub file: File,
    pub size: u64,
    pub sector: u32,
    #[cfg(windows)]
    pub(crate) _volumes: Vec<File>,
}

impl Device {
    // sync_all issues FlushFileBuffers/fsync, flushing the device write cache
    pub fn sync(&self) {
        let _ = self.file.sync_all();
    }

    // ensures the verify pass reads the medium, not a cache: Linux drops the
    // page cache here; Windows handle is NO_BUFFERING; macOS rdisk is uncached
    pub fn prepare_verify(&self) {
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::posix_fadvise(self.file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
}

// 4096-byte alignment required by FILE_FLAG_NO_BUFFERING transfers
pub struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
    len: usize,
}

unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> AlignedBuf {
        let layout = Layout::from_size_align(len, 4096).expect("bad layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory");
        AlignedBuf { ptr, layout, len }
    }
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) }
    }
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

pub fn err_hint(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::PermissionDenied {
        let hint = if cfg!(windows) {
            "run as Administrator"
        } else if cfg!(target_os = "macos") {
            "run with sudo"
        } else {
            "add your user to the 'disk' group or run with sudo"
        };
        io::Error::new(e.kind(), format!("{e}; {hint}"))
    } else {
        e
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
            id: "x".into(),
            name: "SanDisk Ultra".into(),
            size: 1024 * 1024 * 1024,
            removable: true,
            bus: "USB".into(),
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

    #[test]
    fn err_hint_adds_privilege_hint() {
        let e = err_hint(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let hint = if cfg!(windows) {
            "Administrator"
        } else {
            "sudo"
        };
        assert!(e.to_string().contains(hint));

        let other = err_hint(io::Error::other("x"));
        assert_eq!(other.to_string(), "x");
    }
}
