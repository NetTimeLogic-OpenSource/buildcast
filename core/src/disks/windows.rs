use super::{Device, DiskInfo};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, FILE_FLAG_NO_BUFFERING, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
    IOCTL_DISK_GET_DRIVE_GEOMETRY, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_EJECT_MEDIA,
    IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY,
    STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

const BUS_TYPE_USB: i32 = 7;
// CTL_CODE(IOCTL_VOLUME_BASE 'V', 0, METHOD_BUFFERED, FILE_ANY_ACCESS)
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;

fn ioctl(f: &File, code: u32, input: Option<&[u8]>, out: &mut [u8]) -> io::Result<u32> {
    let mut ret = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            code,
            input.map_or(std::ptr::null(), |b| b.as_ptr() as *const c_void),
            input.map_or(0, |b| b.len() as u32),
            if out.is_empty() {
                std::ptr::null_mut()
            } else {
                out.as_mut_ptr() as *mut c_void
            },
            out.len() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn disk_length(f: &File) -> io::Result<u64> {
    let mut out = [0u8; 8];
    ioctl(f, IOCTL_DISK_GET_LENGTH_INFO, None, &mut out)?;
    Ok(i64::from_le_bytes(out) as u64)
}

fn sector_size(f: &File) -> u32 {
    // DISK_GEOMETRY: Cylinders i64, MediaType u32, TracksPerCylinder u32,
    // SectorsPerTrack u32, BytesPerSector u32
    let mut out = [0u8; 24];
    match ioctl(f, IOCTL_DISK_GET_DRIVE_GEOMETRY, None, &mut out) {
        Ok(_) => u32::from_le_bytes(out[20..24].try_into().unwrap()).max(512),
        Err(_) => 512,
    }
}

fn device_number(f: &File) -> io::Result<u32> {
    // STORAGE_DEVICE_NUMBER: DeviceType u32, DeviceNumber u32, PartitionNumber u32
    let mut out = [0u8; 12];
    ioctl(f, IOCTL_STORAGE_GET_DEVICE_NUMBER, None, &mut out)?;
    Ok(u32::from_le_bytes(out[4..8].try_into().unwrap()))
}

pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev: TOKEN_ELEVATION = std::mem::zeroed();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut _ as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elev.TokenIsElevated != 0
    }
}

fn system_disk() -> Option<u32> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let f = OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(format!(r"\\.\{drive}"))
        .ok()?;
    device_number(&f).ok()
}

fn desc_string(buf: &[u8], off: u32) -> String {
    let off = off as usize;
    if off == 0 || off >= buf.len() {
        return String::new();
    }
    buf[off..]
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn list_disks(all: bool, max_bytes: u64) -> Vec<DiskInfo> {
    let sys = system_disk();
    let mut disks = Vec::new();
    for n in 0..64u32 {
        let path = format!(r"\\.\PhysicalDrive{n}");
        let Ok(f) = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
        else {
            continue;
        };
        if Some(n) == sys {
            continue;
        }

        let mut query: STORAGE_PROPERTY_QUERY = unsafe { std::mem::zeroed() };
        query.PropertyId = StorageDeviceProperty;
        query.QueryType = PropertyStandardQuery;
        let qbytes = unsafe {
            std::slice::from_raw_parts(
                &query as *const _ as *const u8,
                size_of::<STORAGE_PROPERTY_QUERY>(),
            )
        };
        let mut out = [0u8; 1024];
        if ioctl(&f, IOCTL_STORAGE_QUERY_PROPERTY, Some(qbytes), &mut out).is_err() {
            continue;
        }
        let desc: STORAGE_DEVICE_DESCRIPTOR =
            unsafe { std::ptr::read_unaligned(out.as_ptr() as *const _) };
        let usb = desc.BusType == BUS_TYPE_USB;
        let removable = desc.RemovableMedia != 0;
        if !all && !(usb || removable) {
            continue;
        }

        let size = disk_length(&f).unwrap_or(0);
        if size == 0 || (!all && size > max_bytes) {
            continue;
        }

        let mut name = format!(
            "{} {}",
            desc_string(&out, desc.VendorIdOffset),
            desc_string(&out, desc.ProductIdOffset)
        )
        .trim()
        .to_string();
        if name.is_empty() {
            name = format!("Disk {n}");
        }
        let bus = match desc.BusType {
            1 => "SCSI",
            3 => "ATA",
            7 => "USB",
            11 => "SATA",
            12 => "SD",
            13 => "MMC",
            17 => "NVMe",
            _ => "other",
        }
        .to_string();
        disks.push(DiskInfo {
            id: path,
            name,
            size,
            removable,
            bus,
        });
    }
    disks
}

// returned handles keep the locks alive
fn lock_volumes(disk: u32) -> io::Result<Vec<File>> {
    let mut held = Vec::new();
    let mut name = [0u16; 512];
    let find = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if find == INVALID_HANDLE_VALUE {
        return Ok(held);
    }
    loop {
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let vol = String::from_utf16_lossy(&name[..len]);
        let vol = vol.trim_end_matches('\\').to_string();

        if let Ok(v) = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&vol)
        {
            // VOLUME_DISK_EXTENTS: NumberOfDiskExtents u32, pad, then
            // DISK_EXTENT { DiskNumber u32, pad, StartingOffset i64, ExtentLength i64 } x N
            let mut out = [0u8; 4096];
            if ioctl(&v, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, &mut out).is_ok() {
                let count = u32::from_le_bytes(out[0..4].try_into().unwrap()) as usize;
                let on_disk = (0..count).any(|i| {
                    let off = 8 + i * 24;
                    off + 4 <= out.len()
                        && u32::from_le_bytes(out[off..off + 4].try_into().unwrap()) == disk
                });
                if on_disk {
                    let mut locked = false;
                    for _ in 0..10 {
                        if ioctl(&v, FSCTL_LOCK_VOLUME, None, &mut []).is_ok() {
                            locked = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    if !locked {
                        unsafe { FindVolumeClose(find) };
                        return Err(io::Error::other(
                            "cannot lock a volume on the target device (files in use?)",
                        ));
                    }
                    let _ = ioctl(&v, FSCTL_DISMOUNT_VOLUME, None, &mut []);
                    held.push(v);
                }
            }
        }

        if unsafe { FindNextVolumeW(find, name.as_mut_ptr(), name.len() as u32) } == 0 {
            break;
        }
    }
    unsafe { FindVolumeClose(find) };
    Ok(held)
}

pub fn eject_disk(dev: &Device, _info: &DiskInfo) -> io::Result<()> {
    // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }
    let allow = [0u8; 1];
    let _ = ioctl(
        &dev.file,
        IOCTL_STORAGE_MEDIA_REMOVAL,
        Some(&allow),
        &mut [],
    );
    ioctl(&dev.file, IOCTL_STORAGE_EJECT_MEDIA, None, &mut []).map(|_| ())
}

pub fn open_disk(info: &DiskInfo, _write: bool) -> io::Result<Device> {
    let n: u32 = info
        .id
        .trim_start_matches(r"\\.\PhysicalDrive")
        .parse()
        .map_err(|_| io::Error::other("bad disk id"))?;
    let volumes = lock_volumes(n)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        // no WRITE_THROUGH: syncs per chunk, 2-5x slower on USB
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(&info.id)?;
    let size = disk_length(&file)?;
    let sector = sector_size(&file);
    Ok(Device {
        file,
        size,
        sector,
        _volumes: volumes,
    })
}
