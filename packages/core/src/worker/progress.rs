use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::disks::human_size;
use crate::{Error, Report};

/// UI wake-up callback, for example egui's `request_repaint`.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Progress shared between a job thread and its observer.
pub type Shared = Arc<Mutex<Progress>>;

/// Error for a progress line that cannot be parsed.
#[derive(Debug, thiserror::Error)]
#[error("malformed progress line")]
pub struct ParseProgressError;

/// Snapshot of a running or finished job.
#[derive(Clone)]
pub struct Progress {
    pub comp_done: u64,
    pub comp_total: u64,
    pub done: u64,
    pub message: String,
    pub speed: f64,
    pub stage: Stage,
    pub total: Option<u64>,
}

/// Progress file through which an elevated CLI reports to its parent.
///
/// The parent requests cancellation by creating a marker file next to it.
pub struct ProgressFile {
    path: PathBuf,
}

/// Transfer rate over a window of about 3 s.
///
/// A cumulative average would hide the slowdown when a drive's SLC cache
/// fills up.
#[derive(Default)]
pub(super) struct SpeedMeter {
    samples: VecDeque<(Instant, u64)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Stage {
    Idle = 0,
    Starting = 1,
    Writing = 2,
    Verifying = 3,
    Reading = 4,
    Done = 5,
    Error = 6,
    Cancelled = 7,
}

impl Progress {
    /// Fraction done, or `None` when unknown.
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(t) if t > 0 => Some((self.done as f64 / t as f64) as f32),
            _ if self.comp_total > 0 => {
                Some((self.comp_done as f64 / self.comp_total as f64) as f32)
            }
            _ => None,
        }
    }

    pub fn idle() -> Progress {
        Progress {
            comp_done: 0,
            comp_total: 0,
            done: 0,
            message: String::new(),
            speed: 0.0,
            stage: Stage::Idle,
            total: None,
        }
    }

    /// Bytes done, total, speed and time left for display.
    pub fn status_line(&self) -> String {
        let speed = if self.speed > 0.0 {
            format!("  ·  {}/s", human_size(self.speed as u64))
        } else {
            String::new()
        };
        match self.total {
            Some(t) if t > 0 => {
                let eta = if self.speed > 0.0 {
                    let secs = t.saturating_sub(self.done) as f64 / self.speed;
                    format!("  ·  {} left", format_eta(secs))
                } else {
                    String::new()
                };
                format!("{} / {}{speed}{eta}", human_size(self.done), human_size(t))
            }
            _ => format!("{} so far{speed}", human_size(self.done)),
        }
    }

    /// Single-line wire format read back by [`FromStr`].
    pub fn to_line(&self) -> String {
        format!(
            "P|{}|{}|{}|{}|{}|{:.0}|{}",
            u8::from(self.stage),
            self.done,
            self.total.map_or(-1, |t| t as i64),
            self.comp_done,
            self.comp_total,
            self.speed,
            self.message.replace(['\n', '\r'], " "),
        )
    }

    pub(super) fn begin(&mut self, stage: Stage, total: Option<u64>, comp_total: u64, msg: &str) {
        *self = Progress {
            comp_total,
            message: msg.into(),
            stage,
            total,
            ..Progress::idle()
        };
    }

    /// Records the outcome of a job. `cancelled` is the message for a
    /// cancelled job.
    pub fn finish(&mut self, result: Result<String, Error>, cancelled: &str) {
        (self.stage, self.message) = match result {
            Ok(msg) => (Stage::Done, msg),
            Err(Error::Cancelled) => (Stage::Cancelled, cancelled.into()),
            Err(err) => (Stage::Error, Report(&err).to_string()),
        };
    }

    pub(super) fn tick(&mut self, done: u64, comp_done: u64, speed: f64) {
        self.done = done;
        self.comp_done = comp_done;
        self.speed = speed;
    }
}

impl FromStr for Progress {
    type Err = ParseProgressError;

    fn from_str(line: &str) -> Result<Progress, ParseProgressError> {
        fn field<T: FromStr>(it: &mut std::str::SplitN<'_, char>) -> Result<T, ParseProgressError> {
            it.next()
                .and_then(|s| s.parse().ok())
                .ok_or(ParseProgressError)
        }

        let mut it = line.trim().splitn(8, '|');
        if it.next() != Some("P") {
            return Err(ParseProgressError);
        }
        let stage = Stage::try_from(field::<u8>(&mut it)?)?;
        let done = field(&mut it)?;
        let total: i64 = field(&mut it)?;
        Ok(Progress {
            comp_done: field(&mut it)?,
            comp_total: field(&mut it)?,
            done,
            speed: field(&mut it)?,
            message: it.next().unwrap_or_default().to_owned(),
            stage,
            total: u64::try_from(total).ok(),
        })
    }
}

impl ProgressFile {
    pub fn cancel_requested(&self) -> bool {
        self.cancel_path().exists()
    }

    pub fn new(path: PathBuf) -> ProgressFile {
        ProgressFile { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&self) -> Option<Progress> {
        std::fs::read_to_string(&self.path).ok()?.parse().ok()
    }

    /// Removes the progress file and the cancel marker.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.cancel_path());
    }

    pub fn request_cancel(&self) {
        let _ = std::fs::File::create(self.cancel_path());
    }

    /// A new path in the temp directory, unique within this machine.
    pub fn unique() -> ProgressFile {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("buildcast-{}-{seq}.progress", std::process::id());
        ProgressFile::new(std::env::temp_dir().join(name))
    }

    /// Writes `p` atomically, so the reader never sees half a line.
    pub fn write(&self, p: &Progress) {
        let tmp = self.sibling(".tmp");
        if std::fs::write(&tmp, p.to_line()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn cancel_path(&self) -> PathBuf {
        self.sibling(".cancel")
    }

    fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(suffix);
        name.into()
    }
}

impl SpeedMeter {
    pub(super) fn update(&mut self, done: u64) -> f64 {
        let now = Instant::now();
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0).as_secs_f64() > 3.0 {
            self.samples.pop_front();
        }
        let (t0, d0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt > 0.2 {
            done.saturating_sub(d0) as f64 / dt
        } else {
            0.0
        }
    }
}

impl Stage {
    pub fn is_terminal(self) -> bool {
        matches!(self, Stage::Done | Stage::Error | Stage::Cancelled)
    }

    /// Short label of a running stage.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Starting => "Preparing",
            Stage::Writing => "Writing",
            Stage::Verifying => "Verifying",
            Stage::Reading => "Reading",
            Stage::Idle | Stage::Done | Stage::Error | Stage::Cancelled => "",
        }
    }
}

impl From<Stage> for u8 {
    fn from(stage: Stage) -> u8 {
        stage as u8
    }
}

impl TryFrom<u8> for Stage {
    type Error = ParseProgressError;

    fn try_from(v: u8) -> Result<Stage, ParseProgressError> {
        Ok(match v {
            0 => Stage::Idle,
            1 => Stage::Starting,
            2 => Stage::Writing,
            3 => Stage::Verifying,
            4 => Stage::Reading,
            5 => Stage::Done,
            6 => Stage::Error,
            7 => Stage::Cancelled,
            _ => return Err(ParseProgressError),
        })
    }
}

fn format_eta(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    const ALL_STAGES: [Stage; 8] = [
        Stage::Idle,
        Stage::Starting,
        Stage::Writing,
        Stage::Verifying,
        Stage::Reading,
        Stage::Done,
        Stage::Error,
        Stage::Cancelled,
    ];

    #[test]
    fn stage_terminal() {
        for s in ALL_STAGES {
            assert_eq!(
                s.is_terminal(),
                matches!(s, Stage::Done | Stage::Error | Stage::Cancelled)
            );
        }
    }

    #[test]
    fn stage_codes_roundtrip() {
        for s in ALL_STAGES {
            assert_eq!(Stage::try_from(u8::from(s)).unwrap(), s);
        }
        assert!(Stage::try_from(99).is_err());
    }

    #[test]
    fn fraction_prefers_total() {
        let mut p = Progress::idle();
        assert_eq!(p.fraction(), None);
        p.total = Some(100);
        p.done = 25;
        assert_eq!(p.fraction(), Some(0.25));
        p.total = None;
        p.comp_total = 200;
        p.comp_done = 50;
        assert_eq!(p.fraction(), Some(0.25));
        p.comp_total = 0;
        assert_eq!(p.fraction(), None);
        p.total = Some(0);
        assert_eq!(p.fraction(), None);
    }

    #[test]
    fn line_roundtrip() {
        let p = Progress {
            comp_done: 7,
            comp_total: 8,
            done: 123,
            message: "hello | with pipe".into(),
            speed: 1000.0,
            stage: Stage::Writing,
            total: Some(456),
        };
        let q: Progress = p.to_line().parse().unwrap();
        assert_eq!(q.stage, Stage::Writing);
        assert_eq!(
            (q.done, q.total, q.comp_done, q.comp_total),
            (123, Some(456), 7, 8)
        );
        assert_eq!(q.speed, 1000.0);
        assert_eq!(q.message, "hello | with pipe");

        let idle: Progress = Progress::idle().to_line().parse().unwrap();
        assert_eq!(idle.total, None);
        assert!("garbage".parse::<Progress>().is_err());
        assert!("".parse::<Progress>().is_err());
    }

    #[test]
    fn progress_file_roundtrip() {
        let file = ProgressFile::unique();
        assert!(file.read().is_none());
        let p = Progress {
            done: 42,
            ..Progress::idle()
        };
        file.write(&p);
        assert_eq!(file.read().unwrap().done, 42);
        assert!(!file.cancel_requested());
        file.request_cancel();
        assert!(file.cancel_requested());
        file.remove();
        assert!(file.read().is_none());
        assert!(!file.cancel_requested());
    }

    #[test]
    fn begin_resets_counters() {
        let mut p = Progress {
            comp_done: 99,
            done: 99,
            speed: 9.9,
            ..Progress::idle()
        };
        p.begin(Stage::Verifying, Some(10), 5, "v");
        assert_eq!(p.stage, Stage::Verifying);
        assert_eq!(
            (p.done, p.total, p.comp_done, p.comp_total),
            (0, Some(10), 0, 5)
        );
        assert_eq!(p.speed, 0.0);
        assert_eq!(p.message, "v");
    }

    #[test]
    fn finish_maps_results_to_stages() {
        let mut p = Progress::idle();
        p.finish(Ok("all good".into()), "stopped");
        assert_eq!((p.stage, p.message.as_str()), (Stage::Done, "all good"));

        p.finish(Err(Error::Cancelled), "stopped");
        assert_eq!((p.stage, p.message.as_str()), (Stage::Cancelled, "stopped"));

        p.finish(Err(Error::from(io::Error::other("bad"))), "stopped");
        assert_eq!((p.stage, p.message.as_str()), (Stage::Error, "bad"));
    }

    #[test]
    fn speed_meter_starts_at_zero() {
        let mut m = SpeedMeter::default();
        assert_eq!(m.update(0), 0.0);
        assert!(m.update(1024) >= 0.0);
    }

    #[test]
    fn eta_formats() {
        assert_eq!(format_eta(f64::NAN), "--:--");
        assert_eq!(format_eta(-1.0), "--:--");
        assert_eq!(format_eta(f64::INFINITY), "--:--");
        assert_eq!(format_eta(0.0), "0:00");
        assert_eq!(format_eta(59.0), "0:59");
        assert_eq!(format_eta(75.0), "1:15");
        assert_eq!(format_eta(3599.0), "59:59");
        assert_eq!(format_eta(3661.0), "1:01:01");
    }

    #[test]
    fn status_lines() {
        let mut p = Progress::idle();
        p.stage = Stage::Writing;
        p.done = 512 * 1024 * 1024;
        p.total = Some(1024 * 1024 * 1024);
        p.speed = (32 * 1024 * 1024) as f64;
        let s = p.status_line();
        assert!(s.contains("512.0 MiB / 1.0 GiB"));
        assert!(s.contains("32.0 MiB/s"));
        assert!(s.contains("left"));

        p.speed = 0.0;
        let s = p.status_line();
        assert!(!s.contains("/s"));
        assert!(!s.contains("left"));

        p.total = None;
        p.done = 10 * 1024 * 1024;
        assert!(p.status_line().contains("10.0 MiB so far"));
    }
}
