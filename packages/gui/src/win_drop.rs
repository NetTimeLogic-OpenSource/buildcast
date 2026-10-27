//! An elevated window gets no OLE drag and drop from an unelevated Explorer
//! (UIPI). WM_DROPFILES can pass the message filter, so the window takes
//! drops through a subclass instead of the winit OLE target.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Ole::RevokeDragDrop;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, SetWindowSubclass,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, MSGFLT_ALLOW, WM_DROPFILES,
};

const WM_COPYGLOBALDATA: u32 = 0x0049;

static DROPPED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

pub(crate) fn install(hwnd: HWND, ctx: egui::Context) {
    let _ = CTX.set(ctx);
    unsafe {
        let _ = RevokeDragDrop(hwnd);
        ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, std::ptr::null_mut());
        ChangeWindowMessageFilterEx(hwnd, WM_COPYGLOBALDATA, MSGFLT_ALLOW, std::ptr::null_mut());
        DragAcceptFiles(hwnd, 1);
        SetWindowSubclass(hwnd, Some(subclass_proc), 1, 0);
    }
}

pub(crate) fn take() -> Vec<PathBuf> {
    std::mem::take(&mut DROPPED.lock().unwrap())
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_DROPFILES {
        let hdrop = wparam as HDROP;
        unsafe {
            let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
            let mut paths = Vec::new();
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                let got = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                if got > 0 {
                    paths.push(PathBuf::from(String::from_utf16_lossy(
                        &buf[..got as usize],
                    )));
                }
            }
            DragFinish(hdrop);
            DROPPED.lock().unwrap().extend(paths);
        }
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
        return 0;
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
