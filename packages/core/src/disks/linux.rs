use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

use super::{Device, DiskInfo};
use crate::Error;

fn sys_read(name: &str, file: &str) -> Option<String> {
    fs::read_to_string(format!("/sys/block/{name}/{file}"))
        .ok()
        .map(|s| s.trim().to_string())
}

// sda1 -> sda, nvme0n1p2 -> nvme0n1, mmcblk0p1 -> mmcblk0
fn base_disk(dev: &str) -> String {
    let d = dev.trim_start_matches("/dev/");
    if d.starts_with("nvme") || d.starts_with("mmcblk") || d.starts_with("loop") {
        if let Some(pos) = d.rfind('p')
            && d[pos + 1..].chars().all(|c| c.is_ascii_digit())
            && !d[pos + 1..].is_empty()
        {
            return d[..pos].to_string();
        }
        d.to_string()
    } else {
        d.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
    }
}

fn root_disk() -> Option<String> {
    let mounts = fs::read_to_string("/proc/mounts").ok()?;
    for line in mounts.lines() {
        let mut it = line.split_whitespace();
        let dev = it.next()?;
        let mnt = it.next()?;
        if mnt == "/" && dev.starts_with("/dev/") {
            return Some(base_disk(dev));
        }
    }
    None
}

pub(super) fn list(all: bool, max_bytes: u64) -> Vec<DiskInfo> {
    let root = root_disk();
    let mut disks = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return disks;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("loop")
            || name.starts_with("ram")
            || name.starts_with("zram")
            || name.starts_with("dm-")
            || name.starts_with("sr")
            || name.starts_with("fd")
        {
            continue;
        }
        if Some(&name) == root.as_ref() {
            continue;
        }
        let removable = sys_read(&name, "removable").as_deref() == Some("1");
        let is_usb = fs::canonicalize(format!("/sys/block/{name}"))
            .map(|p| p.to_string_lossy().contains("/usb"))
            .unwrap_or(false);
        if !all && !(removable || is_usb) {
            continue;
        }
        let size = sys_read(&name, "size")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            * 512;
        if size == 0 || (!all && size > max_bytes) {
            continue;
        }
        let vendor = sys_read(&name, "device/vendor").unwrap_or_default();
        let model = sys_read(&name, "device/model")
            .or_else(|| sys_read(&name, "device/name"))
            .unwrap_or_default();
        let mut label = format!("{vendor} {model}").trim().to_string();
        if label.is_empty() {
            label = name.clone();
        }
        disks.push(DiskInfo {
            bus: if is_usb { "USB".into() } else { String::new() },
            id: format!("/dev/{name}"),
            name: label,
            removable,
            size,
        });
    }
    disks
}

// deepest mount point first, so nested mounts unwind cleanly
fn unmount_partitions(dev_path: &str) -> Result<(), Error> {
    let disk = base_disk(dev_path);
    let mounts = fs::read_to_string("/proc/mounts")?;
    let mut targets: Vec<String> = mounts
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let dev = it.next()?;
            let mnt = it.next()?;
            // /proc/mounts escapes spaces as \040
            (dev.starts_with("/dev/") && base_disk(dev) == disk).then(|| mnt.replace("\\040", " "))
        })
        .collect();
    targets.sort_by_key(|m| std::cmp::Reverse(m.len()));
    for mount in targets {
        let unmounted = CString::new(mount.as_str())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
            .and_then(|c| {
                if unsafe { libc::umount2(c.as_ptr(), 0) } == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        if let Err(source) = unmounted {
            return Err(Error::Unmount { mount, source });
        }
    }
    Ok(())
}

pub(super) fn eject(dev: &Device, info: &DiskInfo) -> Result<(), Error> {
    const CDROMEJECT: libc::c_ulong = 0x5309;
    if unsafe { libc::ioctl(dev.file.as_raw_fd(), CDROMEJECT as _) } == 0 {
        return Ok(());
    }
    match std::process::Command::new("eject").arg(&info.id).status() {
        Ok(s) if s.success() => Ok(()),
        _ => Err(Error::Eject {
            id: info.id.clone(),
        }),
    }
}

pub(super) fn open(info: &DiskInfo, write: bool) -> Result<Device, Error> {
    unmount_partitions(&info.id)?;
    let mut opts = OpenOptions::new();
    opts.read(true);
    if write {
        opts.write(true)
            .custom_flags(libc::O_EXCL | libc::O_SYNC | libc::O_CLOEXEC);
    } else {
        opts.custom_flags(libc::O_EXCL | libc::O_CLOEXEC);
    }
    let mut file = opts.open(&info.id)?;

    let mut sector: libc::c_int = 0;
    let r = unsafe { libc::ioctl(file.as_raw_fd(), libc::BLKSSZGET, &mut sector) };
    let sector = if r == 0 && sector > 0 {
        sector as u32
    } else {
        512
    };

    let size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    file.seek(SeekFrom::Start(0))?;
    let size = if size > 0 { size } else { info.size };

    Ok(Device { file, sector, size })
}
