use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

use buildcast_core::bmap;
use buildcast_core::disks::{self, human_size, DiskInfo};
use buildcast_core::stream::Source;

const EXIT_USAGE: u8 = 1;
const EXIT_DEVICE: u8 = 2;
const EXIT_VERIFY: u8 = 3;
const EXIT_ABORT: u8 = 4;

#[derive(Parser)]
#[command(
    name = "buildcast-cli",
    version,
    about = "Write disk images to drives; supports .bmap for mapped-block flashing"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List candidate target drives
    List {
        /// Include non-removable drives (system disk is never listed)
        #[arg(long)]
        all: bool,
    },
    /// Write an image to a drive
    Copy {
        /// .wic / .wic.gz / .wic.xz / .wic.zst image
        image: PathBuf,
        /// .bmap file; default: <image without compression extension>.bmap
        #[arg(long)]
        bmap: Option<PathBuf>,
        /// Target: number N for \\.\PhysicalDriveN, or a device id from `list`
        #[arg(long)]
        device: String,
        /// Allow non-removable, non-USB targets
        #[arg(long)]
        force: bool,
        /// Skip checksum verification
        #[arg(long)]
        no_verify: bool,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Ignore any bmap and stream-write the full image
        #[arg(long)]
        raw: bool,
        /// Eject the media after a successful write
        #[arg(long)]
        eject: bool,
    },
}

fn device_id(spec: &str) -> String {
    if cfg!(windows) && spec.chars().all(|c| c.is_ascii_digit()) {
        format!(r"\\.\PhysicalDrive{spec}")
    } else {
        spec.to_string()
    }
}

// foo.wic.xz -> foo.wic.bmap
fn default_bmap(image: &Path) -> PathBuf {
    let mut base = image.to_path_buf();
    if let Some(ext) = base.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        if ["gz", "xz", "zst", "bz2", "bzip2"].contains(&ext.as_str()) {
            base.set_extension("");
        }
    }
    PathBuf::from(format!("{}.bmap", base.display()))
}

fn cmd_list(all: bool) -> ExitCode {
    if !disks::is_elevated() {
        eprintln!(
            "warning: not running elevated, drives may be invisible; use an Administrator terminal"
        );
    }
    let list = disks::list_disks(all, u64::MAX);
    if list.is_empty() {
        println!(
            "no candidate drives found{}",
            if all { "" } else { " (try --all)" }
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{:<24} {:>10}  {:<6} {:<9} name",
        "id", "size", "bus", "removable"
    );
    for d in &list {
        println!(
            "{:<24} {:>10}  {:<6} {:<9} {}",
            d.id,
            human_size(d.size),
            d.bus,
            d.removable,
            d.name
        );
    }
    ExitCode::SUCCESS
}

fn confirm(disk: &DiskInfo, image: &Path, image_size: Option<u64>) -> bool {
    println!("about to ERASE and write:");
    println!(
        "  image:  {}{}",
        image.display(),
        image_size
            .map(|s| format!(" ({})", human_size(s)))
            .unwrap_or_default()
    );
    println!("  device: {}  {}", disk.id, disk.label());
    print!("type the device id (or its trailing number) to continue: ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let t = line.trim();
    !t.is_empty() && (t == disk.id || disk.id.ends_with(t))
}

fn progress_bar(total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{bar:40} {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta})")
            .unwrap(),
    );
    pb
}

#[allow(clippy::too_many_arguments)]
fn cmd_copy(
    image: PathBuf,
    bmap_path: Option<PathBuf>,
    device: String,
    force: bool,
    no_verify: bool,
    yes: bool,
    raw: bool,
    eject: bool,
) -> ExitCode {
    if !disks::is_elevated() {
        eprintln!("raw disk access requires elevation, run from an Administrator terminal");
        return ExitCode::from(EXIT_DEVICE);
    }
    let id = device_id(&device);
    let candidates = disks::list_disks(force, u64::MAX);
    let Some(disk) = candidates.iter().find(|d| d.id == id).cloned() else {
        eprintln!(
            "device \"{id}\" not found among candidates{}",
            if force {
                ""
            } else {
                " (non-removable drives need --force)"
            }
        );
        return ExitCode::from(EXIT_DEVICE);
    };

    let bm = if raw {
        None
    } else {
        let path = bmap_path.unwrap_or_else(|| default_bmap(&image));
        let xml = match std::fs::read_to_string(&path) {
            Ok(x) => x,
            Err(e) => {
                eprintln!(
                    "cannot read bmap {}: {e} (use --raw for full writes)",
                    path.display()
                );
                return ExitCode::from(EXIT_USAGE);
            }
        };
        match bmap::parse(&xml) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(EXIT_USAGE);
            }
        }
    };

    let src = match Source::open(&image) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open image: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let needed = bm.as_ref().map(|b| b.image_size).or(src.uncomp_size);
    if let Some(n) = needed {
        if n > disk.size {
            eprintln!(
                "image ({}) is larger than the device ({})",
                human_size(n),
                human_size(disk.size)
            );
            return ExitCode::from(EXIT_DEVICE);
        }
    }

    if !yes && !confirm(&disk, &image, needed) {
        eprintln!("aborted");
        return ExitCode::from(EXIT_ABORT);
    }

    let mut dev = match disks::open_disk(&disk, true).map_err(disks::err_hint) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot open device: {e}");
            return ExitCode::from(EXIT_DEVICE);
        }
    };
    let sector = dev.sector as usize;
    let buf_bytes = 4 * 1024 * 1024;
    #[cfg(windows)]
    ctrl_c::install();
    #[cfg(windows)]
    let cancel: &AtomicBool = &ctrl_c::REQUESTED;
    #[cfg(not(windows))]
    let cancel = &AtomicBool::new(false);
    let started = std::time::Instant::now();

    let result: io::Result<(u64, u64)> = match &bm {
        Some(b) => {
            let pb = progress_bar(b.mapped_bytes());
            let mut reader = src.reader;
            let r = bmap::copy_ranges(
                &mut reader,
                &mut dev.file,
                b,
                sector,
                buf_bytes,
                !no_verify,
                cancel,
                |done| pb.set_position(done),
            );
            pb.finish();
            r.map(|s| (s.written, s.skipped))
        }
        None => {
            let pb = match src.uncomp_size {
                Some(t) => progress_bar(t),
                None => ProgressBar::new_spinner(),
            };
            let mut reader = src.reader;
            let r = raw_copy(&mut reader, &mut dev, sector, buf_bytes, cancel, |done| {
                pb.set_position(done)
            });
            pb.finish();
            r.map(|written| (written, 0))
        }
    };

    let (written, skipped) = match result {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            eprintln!("{e}");
            return ExitCode::from(EXIT_VERIFY);
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            eprintln!("aborted; device contents are undefined");
            return ExitCode::from(EXIT_ABORT);
        }
        Err(e) => {
            eprintln!("write failed: {e}");
            return ExitCode::from(EXIT_DEVICE);
        }
    };
    if let Err(e) = dev.file.flush() {
        eprintln!("flush failed: {e}");
        return ExitCode::from(EXIT_DEVICE);
    }
    dev.sync();
    if eject {
        match disks::eject_disk(&dev, &disk) {
            Ok(()) => println!("media ejected"),
            Err(e) => eprintln!("warning: eject failed ({e}); remove via the system tray"),
        }
    }

    let secs = started.elapsed().as_secs_f64();
    println!(
        "done: {} written, {} skipped, {:.1}s, {}/s",
        human_size(written),
        human_size(skipped),
        secs,
        human_size((written as f64 / secs.max(0.001)) as u64)
    );
    ExitCode::SUCCESS
}

fn raw_copy<R: io::Read>(
    src: &mut R,
    dev: &mut buildcast_core::disks::Device,
    sector: usize,
    buf_bytes: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> io::Result<u64> {
    use buildcast_core::disks::AlignedBuf;
    use buildcast_core::stream::fill;
    let mut buf = AlignedBuf::new(buf_bytes);
    let mut written = 0u64;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = fill(src, buf.as_mut_slice())?;
        if n == 0 {
            break;
        }
        let padded = n.div_ceil(sector) * sector;
        buf.as_mut_slice()[n..padded].fill(0);
        if written + padded as u64 > dev.size {
            return Err(io::Error::other("image is larger than the device"));
        }
        dev.file.write_all(&buf.as_slice()[..padded])?;
        written += n as u64;
        on_progress(written);
    }
    Ok(written)
}

#[cfg(windows)]
mod ctrl_c {
    use std::sync::atomic::{AtomicBool, Ordering};

    pub static REQUESTED: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn routine(_: u32) -> i32 {
        REQUESTED.store(true, Ordering::Relaxed);
        1
    }

    pub fn install() {
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(routine), 1);
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::from(EXIT_USAGE)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    match cli.cmd {
        Cmd::List { all } => cmd_list(all),
        Cmd::Copy {
            image,
            bmap,
            device,
            force,
            no_verify,
            yes,
            raw,
            eject,
        } => cmd_copy(image, bmap, device, force, no_verify, yes, raw, eject),
    }
}
