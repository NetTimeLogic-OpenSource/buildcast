mod decoder;
mod progress;

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use self::decoder::Decoder;
use self::progress::SpeedMeter;
pub use self::progress::{Notify, ParseProgressError, Progress, ProgressFile, Shared, Stage};
use crate::disks::{AlignedBuf, Device, DiskInfo, human_size};
use crate::stream::{Sink, Source};
use crate::{Error, Report, bmap};

/// I/O buffer size in MiB.
///
/// Holding a `BufferSize` guarantees a value from 1 to 512 MiB. Every value
/// is a multiple of the largest supported sector size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferSize(usize);

#[derive(Debug, thiserror::Error)]
#[error("the buffer size must be a whole number of MiB from 1 to 512")]
pub struct InvalidBufferSize;

pub enum Job {
    Read {
        compress: bool,
        output: PathBuf,
    },
    Write {
        bmap: Option<PathBuf>,
        /// Read the device first and skip blocks that already match.
        compare: bool,
        image: PathBuf,
        verify: bool,
    },
}

pub struct JobOpts {
    pub buffer: BufferSize,
    pub eject: bool,
}

impl BufferSize {
    pub const CHOICES: [BufferSize; 10] = [
        BufferSize(1),
        BufferSize(2),
        BufferSize(4),
        BufferSize(8),
        BufferSize(16),
        BufferSize(32),
        BufferSize(64),
        BufferSize(128),
        BufferSize(256),
        BufferSize(512),
    ];

    pub fn bytes(self) -> usize {
        self.0 * 1024 * 1024
    }

    pub fn mib(self) -> usize {
        self.0
    }
}

impl Default for BufferSize {
    fn default() -> BufferSize {
        BufferSize(4)
    }
}

impl FromStr for BufferSize {
    type Err = InvalidBufferSize;

    fn from_str(s: &str) -> Result<BufferSize, InvalidBufferSize> {
        s.trim()
            .parse::<usize>()
            .map_err(|_| InvalidBufferSize)?
            .try_into()
    }
}

impl TryFrom<usize> for BufferSize {
    type Error = InvalidBufferSize;

    fn try_from(mib: usize) -> Result<BufferSize, InvalidBufferSize> {
        if (1..=512).contains(&mib) {
            Ok(BufferSize(mib))
        } else {
            Err(InvalidBufferSize)
        }
    }
}

impl Job {
    /// Result message shown when the job is cancelled.
    pub fn cancel_message(&self) -> &'static str {
        match self {
            Job::Read { .. } => "Cancelled. The partial backup was deleted.",
            Job::Write { .. } => "Cancelled. The device contents are now undefined.",
        }
    }

    /// Runs the job on the calling thread and returns the result message.
    pub fn run(
        &self,
        disk: &DiskInfo,
        opts: &JobOpts,
        prog: &Shared,
        cancel: &AtomicBool,
    ) -> Result<String, Error> {
        let ctx = Context {
            buf_bytes: opts.buffer.bytes(),
            cancel,
            disk,
            eject: opts.eject,
            prog,
        };
        match self {
            Job::Read { compress, output } => ctx.read(output, *compress),
            Job::Write {
                bmap: Some(bmap),
                image,
                verify,
                ..
            } => ctx.write_bmap(image, bmap, *verify),
            Job::Write {
                bmap: None,
                compare,
                image,
                verify,
            } => ctx.write(image, *verify, *compare),
        }
    }

    /// Runs the job on a new thread and stores the outcome in `prog`.
    pub fn spawn(
        self,
        disk: DiskInfo,
        opts: JobOpts,
        prog: Shared,
        cancel: Arc<AtomicBool>,
        notify: Notify,
    ) {
        std::thread::spawn(move || {
            let res = self.run(&disk, &opts, &prog, &cancel);
            prog.lock().unwrap().finish(res, self.cancel_message());
            notify();
        });
    }
}

struct Context<'a> {
    buf_bytes: usize,
    cancel: &'a AtomicBool,
    disk: &'a DiskInfo,
    eject: bool,
    prog: &'a Shared,
}

impl Context<'_> {
    fn begin(&self, stage: Stage, total: Option<u64>, comp_total: u64, msg: &str) {
        self.prog
            .lock()
            .unwrap()
            .begin(stage, total, comp_total, msg);
    }

    fn check_cancel(&self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    // appended to the result message
    fn eject_note(&self, dev: &Device) -> String {
        if !self.eject {
            return String::new();
        }
        match dev.eject(self.disk) {
            Ok(()) => " Media ejected, safe to remove.".into(),
            Err(err) => format!(
                " Eject failed ({}). Eject the media before removing it.",
                Report(&err)
            ),
        }
    }

    fn read(&self, out: &Path, compress: bool) -> Result<String, Error> {
        self.begin(Stage::Starting, None, 0, "Preparing device…");
        let mut dev = Device::open(self.disk, false)?;
        let total = dev.size;
        let mut sink = Sink::create(out, compress, Some(total))?;
        let mut buf = AlignedBuf::new(self.buf_bytes);
        let mut meter = SpeedMeter::default();
        let mut done: u64 = 0;

        self.begin(Stage::Reading, Some(total), 0, "Reading device…");
        while done < total {
            if let Err(err) = self.check_cancel() {
                drop(sink);
                let _ = std::fs::remove_file(out);
                return Err(err);
            }
            let n = (total - done).min(self.buf_bytes as u64) as usize;
            dev.file.read_exact(&mut buf.as_mut_slice()[..n])?;
            sink.write_all(&buf.as_slice()[..n])?;
            done += n as u64;
            self.tick(done, 0, &mut meter);
        }
        sink.finish()?;
        let out_size = std::fs::metadata(out).map_or(0, |m| m.len());
        Ok(format!(
            "Backup saved to {} ({} read, file size {}).{}",
            out.display(),
            human_size(total),
            human_size(out_size),
            self.eject_note(&dev)
        ))
    }

    fn tick(&self, done: u64, comp_done: u64, meter: &mut SpeedMeter) {
        let speed = meter.update(done);
        self.prog.lock().unwrap().tick(done, comp_done, speed);
    }

    fn write(&self, image: &Path, verify: bool, compare: bool) -> Result<String, Error> {
        let src = Source::open(image)?;
        self.begin(
            Stage::Starting,
            src.uncomp_size,
            src.comp_size,
            "Preparing device (unmounting volumes)…",
        );

        let mut dev = Device::open(self.disk, true)?;
        if let Some(size) = src.uncomp_size
            && size > dev.size
        {
            return Err(Error::ImageTooLarge {
                image: size,
                device: dev.size,
            });
        }

        let sector = dev.sector as usize;
        let comp_pos = src.comp_pos.clone();
        let decoder = Decoder::spawn(src.reader, self.buf_bytes);
        let mut cmp = compare.then(|| AlignedBuf::new(self.buf_bytes));
        let mut meter = SpeedMeter::default();
        let mut written: u64 = 0;
        let mut skipped: u64 = 0;

        self.begin(Stage::Writing, src.uncomp_size, src.comp_size, "Writing…");
        loop {
            self.check_cancel()?;
            let mut chunk = decoder.next()?;
            if chunk.len == 0 {
                break;
            }
            let padded = chunk.len.div_ceil(sector) * sector;
            chunk.buf.as_mut_slice()[chunk.len..padded].fill(0);
            if written + padded as u64 > dev.size {
                return Err(Error::ImageTooLarge {
                    image: written + padded as u64,
                    device: dev.size,
                });
            }
            // a reflash of a mostly identical image runs at read speed
            let same = match &mut cmp {
                Some(c) => {
                    dev.file.read_exact(&mut c.as_mut_slice()[..padded])?;
                    c.as_slice()[..padded] == chunk.buf.as_slice()[..padded]
                }
                None => false,
            };
            if same {
                skipped += chunk.len as u64;
            } else {
                if cmp.is_some() {
                    dev.file.seek(SeekFrom::Start(written))?;
                }
                dev.file.write_all(&chunk.buf.as_slice()[..padded])?;
            }
            written += chunk.len as u64;
            decoder.recycle(chunk.buf);
            self.tick(written, comp_pos.load(Ordering::Relaxed), &mut meter);
        }
        dev.file.flush()?;
        dev.sync();
        let skip_note = if skipped > 0 {
            format!(" ({} unchanged and skipped)", human_size(skipped))
        } else {
            String::new()
        };

        if verify {
            self.begin(Stage::Verifying, Some(written), src.comp_size, "Verifying…");
            dev.prepare_verify();
            dev.file.seek(SeekFrom::Start(0))?;
            let src = Source::open(image)?;
            let comp_pos = src.comp_pos.clone();
            let decoder = Decoder::spawn(src.reader, self.buf_bytes);
            let mut vbuf = AlignedBuf::new(self.buf_bytes);
            let mut meter = SpeedMeter::default();
            let mut checked: u64 = 0;
            loop {
                self.check_cancel()?;
                let chunk = decoder.next()?;
                let n = chunk.len;
                if n == 0 {
                    break;
                }
                let padded = n.div_ceil(sector) * sector;
                dev.file.read_exact(&mut vbuf.as_mut_slice()[..padded])?;
                let want = &chunk.buf.as_slice()[..n];
                let got = &vbuf.as_slice()[..n];
                if let Some(pos) = want.iter().zip(got).position(|(a, b)| a != b) {
                    return Err(Error::VerifyMismatch {
                        offset: checked + pos as u64,
                    });
                }
                checked += n as u64;
                decoder.recycle(chunk.buf);
                self.tick(checked, comp_pos.load(Ordering::Relaxed), &mut meter);
            }
        }

        Ok(format!(
            "Done. {} written{}{skip_note}.{}",
            human_size(written),
            if verify { " and verified" } else { "" },
            self.eject_note(&dev)
        ))
    }

    fn write_bmap(&self, image: &Path, bmap_path: &Path, verify: bool) -> Result<String, Error> {
        let bm = bmap::load(bmap_path)?;
        let src = Source::open(image)?;
        let mapped = bm.mapped_bytes();
        self.begin(
            Stage::Starting,
            Some(mapped),
            src.comp_size,
            "Preparing device (unmounting volumes)…",
        );

        let mut dev = Device::open(self.disk, true)?;
        if bm.image_size() > dev.size {
            return Err(Error::ImageTooLarge {
                image: bm.image_size(),
                device: dev.size,
            });
        }

        let sector = dev.sector as usize;
        let comp_pos = src.comp_pos.clone();
        let mut reader = src.reader;
        let mut meter = SpeedMeter::default();

        self.begin(
            Stage::Writing,
            Some(mapped),
            src.comp_size,
            "Writing mapped ranges…",
        );
        let stats = bm.copy_ranges(
            &mut reader,
            &mut dev.file,
            sector,
            self.buf_bytes,
            true,
            self.cancel,
            |done| self.tick(done, comp_pos.load(Ordering::Relaxed), &mut meter),
        )?;
        dev.file.flush()?;
        dev.sync();

        if verify {
            self.begin(
                Stage::Verifying,
                Some(mapped),
                0,
                "Verifying mapped ranges…",
            );
            dev.prepare_verify();
            let mut meter = SpeedMeter::default();
            bm.verify_ranges(&mut dev.file, sector, self.buf_bytes, self.cancel, |done| {
                self.tick(done, 0, &mut meter)
            })?;
        }

        Ok(format!(
            "Done. {} of mapped data written{}, {} unmapped skipped.{}",
            human_size(stats.written),
            if verify { " and verified" } else { "" },
            human_size(stats.skipped),
            self.eject_note(&dev)
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn buffer_size_bounds() {
        assert_eq!(BufferSize::default().mib(), 4);
        assert_eq!("16".parse::<BufferSize>().unwrap().bytes(), 16 << 20);
        assert!("0".parse::<BufferSize>().is_err());
        assert!("513".parse::<BufferSize>().is_err());
        assert!("x".parse::<BufferSize>().is_err());
        for b in BufferSize::CHOICES {
            assert_eq!(BufferSize::try_from(b.mib()).unwrap(), b);
        }
    }

    #[test]
    fn missing_image_fails_before_touching_the_device() {
        let prog: Shared = Arc::new(Mutex::new(Progress::idle()));
        let cancel = AtomicBool::new(false);
        let disk = DiskInfo {
            bus: String::new(),
            id: "nonexistent".into(),
            name: "fake".into(),
            removable: true,
            size: 0,
        };
        let opts = JobOpts {
            buffer: BufferSize::default(),
            eject: false,
        };
        let missing = PathBuf::from("Z:/definitely/not/here.img");
        let write = Job::Write {
            bmap: None,
            compare: false,
            image: missing.clone(),
            verify: false,
        };
        assert!(matches!(
            write.run(&disk, &opts, &prog, &cancel),
            Err(Error::Image(_))
        ));
        let read = Job::Read {
            compress: false,
            output: missing,
        };
        assert!(read.run(&disk, &opts, &prog, &cancel).is_err());
    }
}
