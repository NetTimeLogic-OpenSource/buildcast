use std::process::ExitCode;
use std::time::Duration;

use buildcast_core::elevate::ElevatedProcess;
use buildcast_core::worker::ProgressFile;
use buildcast_core::{Error, Report};

use crate::{Bar, ctrl_c};

/// Runs this command again elevated and mirrors the child's progress
/// file into this console, like sudo.
pub(crate) fn rerun(confirmed: bool) -> ExitCode {
    let file = ProgressFile::unique();
    file.remove();
    let mut args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if !confirmed {
        args.push("--yes".into());
    }
    args.push("--progress-file".into());
    args.push(file.path().display().to_string());

    let child = std::env::current_exe()
        .map_err(Error::from)
        .and_then(|exe| ElevatedProcess::spawn(&exe, &args));
    let child = match child {
        Ok(child) => child,
        Err(err) => {
            eprintln!("{}", Report(&err));
            return ExitCode::from(crate::EXIT_ABORT);
        }
    };

    ctrl_c::install();
    let mut bar = Bar::default();
    let code = loop {
        let exited = child.exit_code();
        if ctrl_c::requested() {
            file.request_cancel();
        }
        if let Some(p) = file.read() {
            bar.update(&p);
        }
        if let Some(code) = exited {
            break code;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    bar.finish();

    match file.read() {
        Some(p) if p.stage.is_terminal() && code == 0 => println!("{}", p.message),
        Some(p) if p.stage.is_terminal() => eprintln!("{}", p.message),
        _ if code != 0 => eprintln!("the elevated helper exited with code {code}"),
        _ => {}
    }
    file.remove();
    ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX))
}
