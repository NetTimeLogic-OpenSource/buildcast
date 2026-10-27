use std::fs::OpenOptions;
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;

use super::{Device, DiskInfo};

fn diskutil_plist(args: &[&str]) -> io::Result<plist::Value> {
    let out = Command::new("diskutil").args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "diskutil {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    plist::from_bytes(&out.stdout).map_err(|e| io::Error::other(format!("plist parse: {e}")))
}

fn media_name(id: &str) -> Option<String> {
    let v = diskutil_plist(&["info", "-plist", id]).ok()?;
    let d = v.as_dictionary()?;
    for key in ["MediaName", "IORegistryEntryName"] {
        if let Some(s) = d.get(key).and_then(|v| v.as_string())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    None
}

pub fn list_disks(all: bool, max_bytes: u64) -> Vec<DiskInfo> {
    let args: &[&str] = if all {
        &["list", "-plist"]
    } else {
        &["list", "-plist", "external", "physical"]
    };
    let mut disks = Vec::new();
    let Ok(v) = diskutil_plist(args) else {
        return disks;
    };
    let Some(list) = v
        .as_dictionary()
        .and_then(|d| d.get("AllDisksAndPartitions"))
        .and_then(|v| v.as_array())
    else {
        return disks;
    };
    for item in list {
        let Some(d) = item.as_dictionary() else {
            continue;
        };
        let Some(id) = d.get("DeviceIdentifier").and_then(|v| v.as_string()) else {
            continue;
        };
        if all && id == "disk0" {
            continue;
        }
        let size = d
            .get("Size")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0);
        if size == 0 || (!all && size > max_bytes) {
            continue;
        }
        let name = media_name(id).unwrap_or_else(|| id.to_string());
        disks.push(DiskInfo {
            id: id.to_string(),
            name,
            size,
            removable: true,
            bus: "external".into(),
        });
    }
    disks
}

pub fn eject_disk(_dev: &Device, info: &DiskInfo) -> io::Result<()> {
    let st = Command::new("diskutil")
        .args(["eject", &format!("/dev/{}", info.id)])
        .status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other("diskutil eject failed"))
    }
}

pub fn open_disk(info: &DiskInfo, write: bool) -> io::Result<Device> {
    let status = Command::new("diskutil")
        .args(["unmountDisk", &format!("/dev/{}", info.id)])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "cannot unmount /dev/{} (files in use?)",
            info.id
        )));
    }
    let mut opts = OpenOptions::new();
    opts.read(true).custom_flags(libc::O_CLOEXEC);
    if write {
        opts.write(true);
    }
    let mut file = opts.open(format!("/dev/r{}", info.id))?;
    let size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    file.seek(SeekFrom::Start(0))?;
    let size = if size > 0 { size } else { info.size };
    Ok(Device {
        file,
        size,
        sector: 512,
    })
}
