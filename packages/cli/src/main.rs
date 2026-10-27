use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

use buildcast_core::disks::{self, DiskInfo, human_size};
use buildcast_core::worker::{self, JobOpts, Progress, Shared, Stage};

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

#[derive(Args)]
struct JobArgs {
    /// Target: number N for \\.\PhysicalDriveN, or a device id from `list`
    #[arg(long)]
    device: String,
    /// Allow non-removable, non-USB targets
    #[arg(long)]
    force: bool,
    /// Skip the confirmation prompt
    #[arg(long)]
    yes: bool,
    /// Eject the media when done
    #[arg(long)]
    eject: bool,
    /// I/O buffer size in MiB
    #[arg(long, default_value_t = 4)]
    buffer_mib: usize,
    /// Write machine-readable progress lines to this file (for host apps)
    #[arg(long, hide = true)]
    progress_file: Option<PathBuf>,
    /// Cancel the job when this file appears
    #[arg(long, hide = true)]
    cancel_file: Option<PathBuf>,
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
        /// Ignore any bmap and stream-write the full image
        #[arg(long)]
        raw: bool,
        /// Skip verification (bmap read-back / full read-back)
        #[arg(long)]
        no_verify: bool,
        /// Write every block without comparing to the device first
        #[arg(long)]
        skip_compare: bool,
        #[command(flatten)]
        job: JobArgs,
    },
    /// Read a drive into a backup image file
    Read {
        /// Output file (.dd, or .dd.zst with --compress)
        output: PathBuf,
        /// zstd-compress the backup
        #[arg(long)]
        compress: bool,
        #[command(flatten)]
        job: JobArgs,
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
    if let Some(ext) = base.extension().map(|e| e.to_string_lossy().to_lowercase())
        && ["gz", "xz", "zst", "bz2", "bzip2"].contains(&ext.as_str())
    {
        base.set_extension("");
    }
    PathBuf::from(format!("{}.bmap", base.display()))
}

fn cmd_list(all: bool) -> ExitCode {
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

fn confirm(disk: &DiskInfo, subject: &str) -> bool {
    println!("{subject}");
    println!("  device: {}  {}", disk.id, disk.label_bus());
    print!("type the device id (or its trailing number) to continue: ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let t = line.trim();
    !t.is_empty() && (t == disk.id || disk.id.ends_with(t))
}

fn find_device(spec: &str, force: bool) -> Result<DiskInfo, ExitCode> {
    let id = device_id(spec);
    let candidates = disks::list_disks(force, u64::MAX);
    candidates
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .ok_or_else(|| {
            eprintln!(
                "device \"{id}\" not found among candidates{}",
                if force {
                    ""
                } else {
                    " (non-removable drives need --force)"
                }
            );
            ExitCode::from(EXIT_DEVICE)
        })
}

fn update_bar(snap: &Progress, bar: &mut Option<ProgressBar>, last_stage: &mut Stage) {
    if snap.stage != *last_stage {
        if let Some(b) = bar.take() {
            b.finish_and_clear();
        }
        if !snap.stage.is_terminal() && snap.stage != Stage::Idle {
            let b = match snap.total {
                Some(t) => ProgressBar::new(t),
                None => ProgressBar::new_spinner(),
            };
            b.set_style(
                ProgressStyle::with_template(
                    "{msg:>9} {bar:40} {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta})",
                )
                .unwrap(),
            );
            b.set_message(format!("{:?}", snap.stage));
            *bar = Some(b);
        }
        *last_stage = snap.stage;
    }
    if let Some(b) = bar {
        if let Some(t) = snap.total {
            b.set_length(t);
        }
        b.set_position(snap.done);
    }
}

// write-then-rename so the host app never reads a half-written snapshot
fn write_progress(path: &Path, text: String) {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// runs the job on a thread while the main thread reports progress to a
// human bar or, for host apps, a machine-readable progress file
fn run_job(
    job: impl FnOnce(&Shared, &AtomicBool) -> io::Result<String> + Send + 'static,
    args: &JobArgs,
) -> ExitCode {
    let prog: Shared = Arc::new(Mutex::new(Progress::idle()));
    let cancel = Arc::new(AtomicBool::new(false));

    #[cfg(windows)]
    ctrl_c::install();
    if let Some(cf) = args.cancel_file.clone() {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            loop {
                if cf.exists() {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }

    let handle = {
        let prog = prog.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || job(&prog, &cancel))
    };

    let machine = args.progress_file.clone();
    let mut bar: Option<ProgressBar> = None;
    let mut last_stage = Stage::Idle;
    while !handle.is_finished() {
        #[cfg(windows)]
        if ctrl_c::REQUESTED.load(Ordering::Relaxed) {
            cancel.store(true, Ordering::Relaxed);
        }
        let snap = prog.lock().unwrap().clone();
        match &machine {
            Some(path) => write_progress(path, worker::format_progress(&snap)),
            None => update_bar(&snap, &mut bar, &mut last_stage),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(b) = bar.take() {
        b.finish_and_clear();
    }

    let result = handle
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("job thread panicked")));
    let (stage, code, msg) = match result {
        Ok(msg) => (Stage::Done, 0u8, msg),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => (
            Stage::Cancelled,
            EXIT_ABORT,
            "cancelled; device contents are undefined".into(),
        ),
        Err(e)
            if e.kind() == io::ErrorKind::InvalidData
                || e.to_string().contains("verification FAILED") =>
        {
            (Stage::Error, EXIT_VERIFY, e.to_string())
        }
        Err(e) => (Stage::Error, EXIT_DEVICE, disks::err_hint(e).to_string()),
    };

    if let Some(path) = &machine {
        let mut p = prog.lock().unwrap().clone();
        p.stage = stage;
        p.message = msg.clone();
        write_progress(path, worker::format_progress(&p));
    }
    if code == 0 {
        println!("{msg}");
    } else {
        eprintln!("{msg}");
    }
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code)
    }
}

fn cmd_copy(
    image: PathBuf,
    bmap: Option<PathBuf>,
    raw: bool,
    no_verify: bool,
    skip_compare: bool,
    args: JobArgs,
) -> ExitCode {
    let disk = match find_device(&args.device, args.force) {
        Ok(d) => d,
        Err(c) => return c,
    };
    let bmap_path = if raw {
        None
    } else {
        let path = bmap.unwrap_or_else(|| default_bmap(&image));
        if !path.is_file() {
            eprintln!(
                "cannot read bmap {} (use --raw for full writes)",
                path.display()
            );
            return ExitCode::from(EXIT_USAGE);
        }
        Some(path)
    };
    if !args.yes
        && !confirm(
            &disk,
            &format!("about to ERASE the device and write {}", image.display()),
        )
    {
        eprintln!("aborted");
        return ExitCode::from(EXIT_ABORT);
    }
    #[cfg(windows)]
    if !disks::is_elevated() {
        let mut eargs = vec![
            "copy".to_string(),
            image.display().to_string(),
            "--device".into(),
            args.device.clone(),
            "--yes".into(),
            "--buffer-mib".into(),
            args.buffer_mib.to_string(),
        ];
        match &bmap_path {
            Some(b) => eargs.extend(["--bmap".into(), b.display().to_string()]),
            None => eargs.push("--raw".into()),
        }
        if no_verify {
            eargs.push("--no-verify".into());
        }
        if skip_compare {
            eargs.push("--skip-compare".into());
        }
        if args.eject {
            eargs.push("--eject".into());
        }
        if args.force {
            eargs.push("--force".into());
        }
        return elevate::rerun(eargs);
    }
    let opts = JobOpts {
        verify: !no_verify,
        buf_bytes: args.buffer_mib.clamp(1, 512) * 1024 * 1024,
        compress: false,
        force: skip_compare,
        eject: args.eject,
    };
    run_job(
        move |prog, cancel| match &bmap_path {
            Some(b) => worker::run_write_bmap(&image, b, &disk, &opts, prog, cancel),
            None => worker::run_write(&image, &disk, &opts, prog, cancel),
        },
        &args,
    )
}

fn cmd_read(output: PathBuf, compress: bool, args: JobArgs) -> ExitCode {
    let disk = match find_device(&args.device, args.force) {
        Ok(d) => d,
        Err(c) => return c,
    };
    if !args.yes
        && !confirm(
            &disk,
            &format!("about to read the device into {}", output.display()),
        )
    {
        eprintln!("aborted");
        return ExitCode::from(EXIT_ABORT);
    }
    #[cfg(windows)]
    if !disks::is_elevated() {
        let mut eargs = vec![
            "read".to_string(),
            output.display().to_string(),
            "--device".into(),
            args.device.clone(),
            "--yes".into(),
            "--buffer-mib".into(),
            args.buffer_mib.to_string(),
        ];
        if compress {
            eargs.push("--compress".into());
        }
        if args.eject {
            eargs.push("--eject".into());
        }
        if args.force {
            eargs.push("--force".into());
        }
        return elevate::rerun(eargs);
    }
    let opts = JobOpts {
        verify: false,
        buf_bytes: args.buffer_mib.clamp(1, 512) * 1024 * 1024,
        compress,
        force: false,
        eject: args.eject,
    };
    run_job(
        move |prog, cancel| worker::run_read(&output, &disk, &opts, prog, cancel),
        &args,
    )
}

// unelevated: re-launch ourselves with the runas verb (one UAC prompt) and
// tail the child's progress file into this console, sudo-style
#[cfg(windows)]
mod elevate {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use buildcast_core::worker::{self, Stage};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn quote(a: &str) -> String {
        if a.contains(' ') || a.contains('"') {
            format!("\"{}\"", a.replace('"', "\\\""))
        } else {
            a.to_string()
        }
    }

    pub fn rerun(mut args: Vec<String>) -> ExitCode {
        let progress =
            std::env::temp_dir().join(format!("buildcast-cli-{}.progress", std::process::id()));
        let cancel = PathBuf::from(format!("{}.cancel", progress.display()));
        let _ = std::fs::remove_file(&progress);
        let _ = std::fs::remove_file(&cancel);
        args.extend([
            "--progress-file".into(),
            progress.display().to_string(),
            "--cancel-file".into(),
            cancel.display().to_string(),
        ]);

        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("cannot locate own executable: {e}");
                return ExitCode::from(crate::EXIT_DEVICE);
            }
        };
        let params = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        let file_w = wide(&exe.display().to_string());
        let params_w = wide(&params);
        let verb_w = wide("runas");

        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb_w.as_ptr();
        info.lpFile = file_w.as_ptr();
        info.lpParameters = params_w.as_ptr();
        info.nShow = 0; // SW_HIDE

        if unsafe { ShellExecuteExW(&mut info) } == 0 || info.hProcess.is_null() {
            eprintln!("elevation was declined");
            return ExitCode::from(crate::EXIT_ABORT);
        }
        let process = info.hProcess;

        super::ctrl_c::install();
        let mut bar = None;
        let mut last_stage = Stage::Idle;
        loop {
            let exited = unsafe { WaitForSingleObject(process, 0) } == 0;
            if super::ctrl_c::REQUESTED.load(Ordering::Relaxed) {
                let _ = std::fs::File::create(&cancel);
            }
            if let Ok(text) = std::fs::read_to_string(&progress)
                && let Some(p) = worker::parse_progress(&text)
            {
                crate::update_bar(&p, &mut bar, &mut last_stage);
            }
            if exited {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(b) = bar.take() {
            b.finish_and_clear();
        }

        let mut code = 0u32;
        unsafe {
            GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
        }
        match std::fs::read_to_string(&progress)
            .ok()
            .and_then(|t| worker::parse_progress(&t))
        {
            Some(p) if p.stage.is_terminal() => {
                if code == 0 {
                    println!("{}", p.message);
                } else {
                    eprintln!("{}", p.message);
                }
            }
            _ if code != 0 => eprintln!("helper exited with code {code}"),
            _ => {}
        }
        let _ = std::fs::remove_file(&progress);
        let _ = std::fs::remove_file(&cancel);
        ExitCode::from(code.min(255) as u8)
    }
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
            raw,
            no_verify,
            skip_compare,
            job,
        } => cmd_copy(image, bmap, raw, no_verify, skip_compare, job),
        Cmd::Read {
            output,
            compress,
            job,
        } => cmd_read(output, compress, job),
    }
}
