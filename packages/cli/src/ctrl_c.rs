use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

static REQUESTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn install() {
    unsafe { SetConsoleCtrlHandler(Some(routine), 1) };
}

pub(crate) fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

unsafe extern "system" fn routine(_: u32) -> i32 {
    REQUESTED.store(true, Ordering::Relaxed);
    1
}
