#[cfg(windows)]
mod ctrl_c;
#[cfg(windows)]
mod elevate;

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use buildcast_core::disks::{DiskInfo, human_size};
use buildcast_core::worker::{BufferSize, Job, JobOpts, Progress, ProgressFile, Shared, Stage};
use buildcast_core::{Error, bmap};
use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

const EXIT_ABORT: u8 = 4;
const EXIT_DEVICE: u8 = 2;
const EXIT_USAGE: u8 = 1;
const EXIT_VERIFY: u8 = 3;

/// Terminal progress bar that restarts on every stage change.
#[derive(Default)]
struct Bar {
    bar: Option<ProgressBar>,
    stage: Option<Stage>,
}

#[derive(Parser)]
#[command(
    name = "buildcast-cli",
    version,
    about = "Write disk images to drives, with optional .bmap mapped-block flashing"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List candidate target drives
    List {
        /// Include non-removable drives (the system disk is never listed)
        #[arg(long)]
        all: bool,
    },
    /// Write an image to a drive
    Copy {
        /// .wic, .wic.gz, .wic.xz or .wic.zst image
        image: PathBuf,
        /// .bmap file [default: foo.wic.bmap for foo.wic.xz]
        #[arg(long)]
        bmap: Option<PathBuf>,
        /// Ignore any bmap and write the full image
        #[arg(long)]
        raw: bool,
        /// Skip the read-back verification
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
        /// Compress the backup with zstd
        #[arg(long)]
        compress: bool,
        #[command(flatten)]
        job: JobArgs,
    },
}

#[derive(Args)]
struct JobArgs {
    /// Target: a number N for \\.\PhysicalDriveN, or a device id from `list`
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
    /// I/O buffer size in MiB (1 to 512)
    #[arg(long, value_name = "MIB", default_value = "4")]
    buffer_mib: BufferSize,
    /// Write machine-readable progress to this file (for host apps)
    #[arg(long, hide = true)]
    progress_file: Option<PathBuf>,
}

impl Bar {
    fn finish(&mut self) {
        if let Some(b) = self.bar.take() {
            b.finish_and_clear();
        }
    }

    fn update(&mut self, snap: &Progress) {
        if self.stage != Some(snap.stage) {
            self.finish();
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
                b.set_message(snap.stage.label());
                self.bar = Some(b);
            }
            self.stage = Some(snap.stage);
        }
        if let Some(b) = &self.bar {
            if let Some(t) = snap.total {
                b.set_length(t);
            }
            b.set_position(snap.done);
        }
    }
}

impl Cmd {
    fn run(self) -> ExitCode {
        match self {
            Cmd::Copy {
                image,
                bmap,
                raw,
                no_verify,
                skip_compare,
                job,
            } => {
                let bmap = if raw {
                    None
                } else {
                    let path = bmap.unwrap_or_else(|| bmap::sidecar_path(&image));
                    if !path.is_file() {
                        eprintln!(
                            "cannot read bmap {} (use --raw for a full write)",
                            path.display()
                        );
                        return ExitCode::from(EXIT_USAGE);
                    }
                    Some(path)
                };
                let subject = format!("about to ERASE the device and write {}", image.display());
                job.start(
                    Job::Write {
                        bmap,
                        compare: !skip_compare,
                        image,
                        verify: !no_verify,
                    },
                    &subject,
                )
            }
            Cmd::List { all } => {
                list(all);
                ExitCode::SUCCESS
            }
            Cmd::Read {
                output,
                compress,
                job,
            } => {
                let subject = format!("about to read the device into {}", output.display());
                job.start(Job::Read { compress, output }, &subject)
            }
        }
    }
}

impl JobArgs {
    fn confirm(&self, disk: &DiskInfo, subject: &str) -> bool {
        if self.yes {
            return true;
        }
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

    fn device_id(&self) -> String {
        if cfg!(windows) && self.device.chars().all(|c| c.is_ascii_digit()) {
            format!(r"\\.\PhysicalDrive{}", self.device)
        } else {
            self.device.clone()
        }
    }

    fn find_device(&self) -> Option<DiskInfo> {
        let id = self.device_id();
        let found = DiskInfo::list(self.force, u64::MAX)
            .into_iter()
            .find(|d| d.id == id);
        if found.is_none() {
            let hint = if self.force {
                ""
            } else {
                " (non-removable drives need --force)"
            };
            eprintln!("device \"{id}\" not found among the candidates{hint}");
        }
        found
    }

    // runs the job on a thread while this thread reports progress to a
    // terminal bar or, for host apps, to a progress file
    fn run(&self, job: Job, disk: DiskInfo) -> ExitCode {
        let prog: Shared = Arc::new(Mutex::new(Progress::idle()));
        let cancel = Arc::new(AtomicBool::new(false));
        let file = self.progress_file.clone().map(ProgressFile::new);

        #[cfg(windows)]
        ctrl_c::install();

        let opts = JobOpts {
            buffer: self.buffer_mib,
            eject: self.eject,
        };
        let handle = {
            let (prog, cancel) = (prog.clone(), cancel.clone());
            std::thread::spawn(move || {
                let res = job.run(&disk, &opts, &prog, &cancel);
                (job, res)
            })
        };

        let mut bar = Bar::default();
        while !handle.is_finished() {
            #[cfg(windows)]
            if ctrl_c::requested() {
                cancel.store(true, Ordering::Relaxed);
            }
            if file.as_ref().is_some_and(ProgressFile::cancel_requested) {
                cancel.store(true, Ordering::Relaxed);
            }
            let snap = prog.lock().unwrap().clone();
            match &file {
                Some(f) => f.write(&snap),
                None => bar.update(&snap),
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bar.finish();

        let (job, res) = handle.join().expect("job thread panicked");
        let code = match &res {
            Ok(_) => 0,
            Err(Error::Cancelled) => EXIT_ABORT,
            Err(err) if err.is_mismatch() => EXIT_VERIFY,
            Err(_) => EXIT_DEVICE,
        };
        let mut snap = prog.lock().unwrap().clone();
        snap.finish(res, job.cancel_message());
        if let Some(f) = &file {
            f.write(&snap);
        }
        if code == 0 {
            println!("{}", snap.message);
            ExitCode::SUCCESS
        } else {
            eprintln!("{}", snap.message);
            ExitCode::from(code)
        }
    }

    fn start(&self, job: Job, subject: &str) -> ExitCode {
        let Some(disk) = self.find_device() else {
            return ExitCode::from(EXIT_DEVICE);
        };
        if !self.confirm(&disk, subject) {
            eprintln!("aborted");
            return ExitCode::from(EXIT_ABORT);
        }
        #[cfg(windows)]
        if !buildcast_core::disks::is_elevated() {
            return elevate::rerun(self.yes);
        }
        self.run(job, disk)
    }
}

fn list(all: bool) {
    let disks = DiskInfo::list(all, u64::MAX);
    if disks.is_empty() {
        let hint = if all { "" } else { " (try --all)" };
        println!("no candidate drives found{hint}");
        return;
    }
    println!(
        "{:<24} {:>10}  {:<6} {:<9} name",
        "id", "size", "bus", "removable"
    );
    for d in &disks {
        println!(
            "{:<24} {:>10}  {:<6} {:<9} {}",
            d.id,
            human_size(d.size),
            d.bus,
            d.removable,
            d.name
        );
    }
}

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => cli.cmd.run(),
        Err(err) => {
            let _ = err.print();
            if err.use_stderr() {
                ExitCode::from(EXIT_USAGE)
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}
