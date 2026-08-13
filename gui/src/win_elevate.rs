// The GUI runs unelevated; device jobs run in buildcast-cli launched with
// the runas verb (one UAC prompt per job). Progress flows back through a
// temp file the CLI rewrites each tick; cancel = creating a marker file.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use buildcast_core::worker::{self, Notify, Shared, Stage};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};

pub struct RemoteJob {
    pub progress_path: PathBuf,
}

impl RemoteJob {
    pub fn cancel(&self) {
        let _ = std::fs::File::create(self.cancel_path());
    }

    pub fn cancel_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.cancel", self.progress_path.display()))
    }
}

pub fn cli_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let cli = exe
        .parent()
        .ok_or_else(|| io::Error::other("no exe dir"))?
        .join("buildcast-cli.exe");
    if cli.is_file() {
        Ok(cli)
    } else {
        Err(io::Error::other(format!(
            "buildcast-cli.exe not found next to the app ({})",
            cli.display()
        )))
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn quote(arg: &str) -> String {
    if arg.contains(' ') || arg.contains('"') {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

// exit codes 2/3/4 map to messages only if the progress file lacks a
// terminal line (e.g. the process died before writing one)
fn fallback_message(code: u32) -> (Stage, String) {
    match code {
        0 => (Stage::Done, "Done".into()),
        4 => (Stage::Cancelled, "Cancelled".into()),
        3 => (Stage::Error, "verification failed".into()),
        c => (Stage::Error, format!("helper exited with code {c}")),
    }
}

pub fn spawn(args: Vec<String>, job: &RemoteJob, prog: Shared, notify: Notify) -> io::Result<()> {
    let cli = cli_path()?;
    let _ = std::fs::remove_file(&job.progress_path);
    let _ = std::fs::remove_file(job.cancel_path());

    let params = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
    let file_w = wide(&cli.display().to_string());
    let params_w = wide(&params);
    let verb_w = wide("runas");

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = verb_w.as_ptr();
    info.lpFile = file_w.as_ptr();
    info.lpParameters = params_w.as_ptr();
    info.nShow = 0; // SW_HIDE: no console window

    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 || info.hProcess.is_null() {
        return Err(io::Error::other(
            "elevation was declined or the helper could not start",
        ));
    }

    let process = info.hProcess as isize;
    let progress_path = job.progress_path.clone();
    std::thread::spawn(move || {
        let process = process as *mut core::ffi::c_void;
        loop {
            let exited = unsafe { WaitForSingleObject(process, 0) } == 0;
            if let Ok(text) = std::fs::read_to_string(&progress_path)
                && let Some(p) = worker::parse_progress(&text)
            {
                *prog.lock().unwrap() = p;
            }
            notify();
            if exited {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let mut code = 0u32;
        unsafe {
            GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
        }
        {
            let mut p = prog.lock().unwrap();
            if !p.stage.is_terminal() {
                let (stage, msg) = fallback_message(code);
                p.stage = stage;
                p.message = msg;
            }
        }
        let _ = std::fs::remove_file(&progress_path);
        notify();
    });
    Ok(())
}
