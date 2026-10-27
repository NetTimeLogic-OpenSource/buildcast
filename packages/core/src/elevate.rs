//! Launching helpers with the `runas` verb, which shows one UAC prompt.

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};

use crate::Error;

/// Elevated child process. The handle closes on drop.
pub struct ElevatedProcess {
    handle: HANDLE,
}

// a process handle may be used from any thread
unsafe impl Send for ElevatedProcess {}

impl ElevatedProcess {
    /// Exit code of the process, or `None` while it runs.
    pub fn exit_code(&self) -> Option<u32> {
        if unsafe { WaitForSingleObject(self.handle, 0) } != 0 {
            return None;
        }
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.handle, &mut code) };
        Some(code)
    }

    /// Starts `exe` elevated, without a console window, in the current
    /// directory.
    pub fn spawn(exe: &Path, args: &[String]) -> Result<ElevatedProcess, Error> {
        let params = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        let file_w = wide(&exe.display().to_string());
        let params_w = wide(&params);
        let verb_w = wide("runas");
        // runas starts in System32, relative paths need the caller's directory
        let dir_w = std::env::current_dir()
            .ok()
            .map(|d| wide(&d.display().to_string()));

        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb_w.as_ptr();
        info.lpFile = file_w.as_ptr();
        info.lpParameters = params_w.as_ptr();
        info.lpDirectory = dir_w.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
        // SW_HIDE
        info.nShow = 0;

        if unsafe { ShellExecuteExW(&mut info) } == 0 || info.hProcess.is_null() {
            return Err(Error::ElevationDeclined);
        }
        Ok(ElevatedProcess {
            handle: info.hProcess,
        })
    }
}

impl Drop for ElevatedProcess {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn quote(arg: &str) -> String {
    if arg.contains(' ') || arg.contains('"') {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
