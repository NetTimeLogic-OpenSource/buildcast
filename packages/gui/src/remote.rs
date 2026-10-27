//! On Windows the GUI runs unelevated. Device jobs run in buildcast-cli,
//! started with the runas verb (one UAC prompt per job). Progress flows back
//! through a progress file.

use std::path::PathBuf;
use std::time::Duration;

use buildcast_core::elevate::ElevatedProcess;
use buildcast_core::worker::{Notify, ProgressFile, Shared, Stage};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Core(#[from] buildcast_core::Error),
    #[error("{} not found next to the app", path.display())]
    HelperMissing { path: PathBuf },
}

/// Elevated CLI job.
pub(crate) struct RemoteJob {
    file: ProgressFile,
}

impl RemoteJob {
    pub(crate) fn cancel(&self) {
        self.file.request_cancel();
    }

    /// Starts `buildcast-cli` with `args` and mirrors its progress into
    /// `prog` until it exits.
    pub(crate) fn spawn(
        mut args: Vec<String>,
        prog: Shared,
        notify: Notify,
    ) -> Result<RemoteJob, Error> {
        let exe = std::env::current_exe().map_err(buildcast_core::Error::from)?;
        let cli = exe.with_file_name("buildcast-cli.exe");
        if !cli.is_file() {
            return Err(Error::HelperMissing { path: cli });
        }
        let file = ProgressFile::unique();
        file.remove();
        args.extend(["--progress-file".into(), file.path().display().to_string()]);
        let child = ElevatedProcess::spawn(&cli, &args)?;

        let watched = ProgressFile::new(file.path().to_path_buf());
        std::thread::spawn(move || {
            let code = loop {
                let exited = child.exit_code();
                if let Some(p) = watched.read() {
                    *prog.lock().unwrap() = p;
                }
                notify();
                if let Some(code) = exited {
                    break code;
                }
                std::thread::sleep(Duration::from_millis(150));
            };
            {
                let mut p = prog.lock().unwrap();
                if !p.stage.is_terminal() {
                    (p.stage, p.message) = fallback(code);
                }
            }
            watched.remove();
            notify();
        });
        Ok(RemoteJob { file })
    }
}

// used only when the CLI died before it wrote a final progress line
fn fallback(code: u32) -> (Stage, String) {
    match code {
        0 => (Stage::Done, "Done.".into()),
        4 => (Stage::Cancelled, "Cancelled.".into()),
        3 => (Stage::Error, "Verification failed.".into()),
        c => (Stage::Error, format!("The helper exited with code {c}.")),
    }
}
