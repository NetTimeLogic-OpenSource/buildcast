use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use crate::bmap;
use crate::disks::{self, AlignedBuf, DiskInfo};
use crate::stream::{Sink, Source, fill};

// UI wake-up callback, e.g. egui's request_repaint
pub type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Idle,
    Starting,
    Writing,
    Verifying,
    Reading,
    Done,
    Error,
    Cancelled,
}

impl Stage {
    pub fn is_terminal(self) -> bool {
        matches!(self, Stage::Done | Stage::Error | Stage::Cancelled)
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Stage::Idle => 0,
            Stage::Starting => 1,
            Stage::Writing => 2,
            Stage::Verifying => 3,
            Stage::Reading => 4,
            Stage::Done => 5,
            Stage::Error => 6,
            Stage::Cancelled => 7,
        }
    }

    pub fn from_u8(v: u8) -> Stage {
        match v {
            1 => Stage::Starting,
            2 => Stage::Writing,
            3 => Stage::Verifying,
            4 => Stage::Reading,
            5 => Stage::Done,
            6 => Stage::Error,
            7 => Stage::Cancelled,
            _ => Stage::Idle,
        }
    }
}

// single-line wire format for the elevated CLI -> GUI progress file
pub fn format_progress(p: &Progress) -> String {
    format!(
        "P|{}|{}|{}|{}|{}|{:.0}|{}",
        p.stage.as_u8(),
        p.done,
        p.total.map(|t| t as i64).unwrap_or(-1),
        p.comp_done,
        p.comp_total,
        p.speed,
        p.message.replace(['\n', '\r'], " "),
    )
}

pub fn parse_progress(line: &str) -> Option<Progress> {
    let mut it = line.trim().splitn(8, '|');
    if it.next()? != "P" {
        return None;
    }
    let stage = Stage::from_u8(it.next()?.parse().ok()?);
    let done = it.next()?.parse().ok()?;
    let total: i64 = it.next()?.parse().ok()?;
    let comp_done = it.next()?.parse().ok()?;
    let comp_total = it.next()?.parse().ok()?;
    let speed = it.next()?.parse().ok()?;
    let message = it.next().unwrap_or("").to_string();
    Some(Progress {
        stage,
        done,
        total: (total >= 0).then_some(total as u64),
        comp_done,
        comp_total,
        speed,
        message,
    })
}

#[derive(Clone)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: Option<u64>,
    pub comp_done: u64,
    pub comp_total: u64,
    pub speed: f64,
    pub message: String,
}

impl Progress {
    pub fn idle() -> Progress {
        Progress {
            stage: Stage::Idle,
            done: 0,
            total: None,
            comp_done: 0,
            comp_total: 0,
            speed: 0.0,
            message: String::new(),
        }
    }

    // None = indeterminate
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(t) if t > 0 => Some((self.done as f64 / t as f64) as f32),
            _ if self.comp_total > 0 => {
                Some((self.comp_done as f64 / self.comp_total as f64) as f32)
            }
            _ => None,
        }
    }
}

pub type Shared = Arc<Mutex<Progress>>;

pub struct JobOpts {
    pub verify: bool,
    pub buf_bytes: usize,
    pub compress: bool,
    // skip the read-compare pass and always write every block
    pub force: bool,
    pub eject: bool,
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "cancelled by user")
}

fn maybe_eject(dev: &disks::Device, disk: &DiskInfo, want: bool) -> String {
    if !want {
        return String::new();
    }
    match disks::eject_disk(dev, disk) {
        Ok(()) => "; media ejected, safe to remove".into(),
        Err(e) => format!("; eject failed ({e}), remove via the system tray"),
    }
}

// len == 0 marks end of stream
struct Chunk {
    buf: AlignedBuf,
    len: usize,
}

// Decompression runs on its own thread so it overlaps device I/O; buffers
// rotate through a pool channel. Dropping either channel end shuts it down.
fn spawn_decoder(
    mut reader: Box<dyn Read + Send>,
    buf_bytes: usize,
) -> (
    mpsc::Receiver<io::Result<Chunk>>,
    mpsc::SyncSender<AlignedBuf>,
) {
    const DEPTH: usize = 3;
    let (data_tx, data_rx) = mpsc::sync_channel::<io::Result<Chunk>>(DEPTH);
    let (pool_tx, pool_rx) = mpsc::sync_channel::<AlignedBuf>(DEPTH);
    for _ in 0..DEPTH {
        pool_tx.send(AlignedBuf::new(buf_bytes)).unwrap();
    }
    std::thread::spawn(move || {
        loop {
            let Ok(mut buf) = pool_rx.recv() else { break };
            match fill(&mut reader, buf.as_mut_slice()) {
                Ok(0) => {
                    let _ = data_tx.send(Ok(Chunk { buf, len: 0 }));
                    break;
                }
                Ok(n) => {
                    if data_tx.send(Ok(Chunk { buf, len: n })).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = data_tx.send(Err(e));
                    break;
                }
            }
        }
    });
    (data_rx, pool_tx)
}

fn next_chunk(rx: &mpsc::Receiver<io::Result<Chunk>>) -> io::Result<Chunk> {
    match rx.recv() {
        Ok(r) => r,
        Err(_) => Err(io::Error::other("decoder thread terminated unexpectedly")),
    }
}

fn finish(res: io::Result<String>, prog: &Shared, notify: &Notify) {
    let mut p = prog.lock().unwrap();
    match res {
        Ok(msg) => {
            p.stage = Stage::Done;
            p.message = msg;
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            p.stage = Stage::Cancelled;
            p.message = "Cancelled; device contents are undefined".into();
        }
        Err(e) => {
            p.stage = Stage::Error;
            p.message = disks::err_hint(e).to_string();
        }
    }
    notify();
}

pub fn spawn_write(
    image: PathBuf,
    disk: DiskInfo,
    opts: JobOpts,
    prog: Shared,
    cancel: Arc<AtomicBool>,
    notify: Notify,
) {
    std::thread::spawn(move || {
        let res = run_write(&image, &disk, &opts, &prog, &cancel);
        finish(res, &prog, &notify);
    });
}

pub fn spawn_write_bmap(
    image: PathBuf,
    bmap_path: PathBuf,
    disk: DiskInfo,
    opts: JobOpts,
    prog: Shared,
    cancel: Arc<AtomicBool>,
    notify: Notify,
) {
    std::thread::spawn(move || {
        let res = run_write_bmap(&image, &bmap_path, &disk, &opts, &prog, &cancel);
        finish(res, &prog, &notify);
    });
}

pub fn spawn_read(
    out: PathBuf,
    disk: DiskInfo,
    opts: JobOpts,
    prog: Shared,
    cancel: Arc<AtomicBool>,
    notify: Notify,
) {
    std::thread::spawn(move || {
        let res = run_read(&out, &disk, &opts, &prog, &cancel);
        finish(res, &prog, &notify);
    });
}

fn set_stage(prog: &Shared, stage: Stage, total: Option<u64>, comp_total: u64, msg: &str) {
    let mut p = prog.lock().unwrap();
    p.stage = stage;
    p.done = 0;
    p.total = total;
    p.comp_done = 0;
    p.comp_total = comp_total;
    p.speed = 0.0;
    p.message = msg.into();
}

// windowed ~3 s rate; a cumulative average masks SLC-cache slowdown
struct SpeedMeter {
    samples: std::collections::VecDeque<(Instant, u64)>,
}

impl SpeedMeter {
    fn new() -> SpeedMeter {
        SpeedMeter {
            samples: std::collections::VecDeque::new(),
        }
    }

    fn update(&mut self, done: u64) -> f64 {
        let now = Instant::now();
        self.samples.push_back((now, done));
        while self.samples.len() > 2
            && now
                .duration_since(self.samples.front().unwrap().0)
                .as_secs_f64()
                > 3.0
        {
            self.samples.pop_front();
        }
        let (t0, d0) = *self.samples.front().unwrap();
        let dt = now.duration_since(t0).as_secs_f64();
        if dt > 0.2 {
            done.saturating_sub(d0) as f64 / dt
        } else {
            0.0
        }
    }
}

fn tick(prog: &Shared, done: u64, comp_done: u64, meter: &mut SpeedMeter) {
    let speed = meter.update(done);
    let mut p = prog.lock().unwrap();
    p.done = done;
    p.comp_done = comp_done;
    p.speed = speed;
}

pub fn run_write(
    image: &std::path::Path,
    disk: &DiskInfo,
    opts: &JobOpts,
    prog: &Shared,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let src = Source::open(image)?;
    set_stage(
        prog,
        Stage::Starting,
        src.uncomp_size,
        src.comp_size,
        "Preparing device (unmounting volumes)…",
    );

    let mut dev = disks::open_disk(disk, true).map_err(disks::err_hint)?;
    if let Some(t) = src.uncomp_size
        && t > dev.size
    {
        return Err(io::Error::other(format!(
            "image ({}) is larger than the device ({})",
            disks::human_size(t),
            disks::human_size(dev.size)
        )));
    }

    let sector = dev.sector as usize;
    let comp_pos = src.comp_pos.clone();
    let (rx, pool) = spawn_decoder(src.reader, opts.buf_bytes);
    let mut cmp = (!opts.force).then(|| AlignedBuf::new(opts.buf_bytes));
    let mut meter = SpeedMeter::new();
    let mut written: u64 = 0;
    let mut skipped: u64 = 0;

    set_stage(
        prog,
        Stage::Writing,
        src.uncomp_size,
        src.comp_size,
        "Writing…",
    );
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let mut chunk = next_chunk(&rx)?;
        if chunk.len == 0 {
            break;
        }
        let padded = chunk.len.div_ceil(sector) * sector;
        chunk.buf.as_mut_slice()[chunk.len..padded].fill(0);
        if written + padded as u64 > dev.size {
            return Err(io::Error::other(format!(
                "image is larger than the device ({})",
                disks::human_size(dev.size)
            )));
        }
        // read-compare, skip identical blocks; reflash runs at read speed
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
        let _ = pool.try_send(chunk.buf);
        tick(prog, written, comp_pos.load(Ordering::Relaxed), &mut meter);
    }
    dev.file.flush()?;
    dev.sync();
    let skip_note = if skipped > 0 {
        format!(" ({} unchanged, skipped)", disks::human_size(skipped))
    } else {
        String::new()
    };

    if opts.verify {
        set_stage(
            prog,
            Stage::Verifying,
            Some(written),
            src.comp_size,
            "Verifying…",
        );
        dev.prepare_verify();
        dev.file.seek(SeekFrom::Start(0))?;
        let src2 = Source::open(image)?;
        let comp_pos2 = src2.comp_pos.clone();
        let (vrx, vpool) = spawn_decoder(src2.reader, opts.buf_bytes);
        let mut vbuf = AlignedBuf::new(opts.buf_bytes);
        let mut vmeter = SpeedMeter::new();
        let mut checked: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let chunk = next_chunk(&vrx)?;
            if chunk.len == 0 {
                break;
            }
            let n = chunk.len;
            let padded = n.div_ceil(sector) * sector;
            dev.file.read_exact(&mut vbuf.as_mut_slice()[..padded])?;
            if chunk.buf.as_slice()[..n] != vbuf.as_slice()[..n] {
                let off = checked
                    + chunk.buf.as_slice()[..n]
                        .iter()
                        .zip(vbuf.as_slice()[..n].iter())
                        .position(|(a, b)| a != b)
                        .unwrap_or(0) as u64;
                return Err(io::Error::other(format!(
                    "verification FAILED at byte offset {off}; device data does not match image"
                )));
            }
            checked += n as u64;
            let _ = vpool.try_send(chunk.buf);
            tick(
                prog,
                checked,
                comp_pos2.load(Ordering::Relaxed),
                &mut vmeter,
            );
        }
        let eject_note = maybe_eject(&dev, disk, opts.eject);
        Ok(format!(
            "Done; {} written and verified{skip_note}{eject_note}",
            disks::human_size(written)
        ))
    } else {
        let eject_note = maybe_eject(&dev, disk, opts.eject);
        Ok(format!(
            "Done; {} written{skip_note}{eject_note}",
            disks::human_size(written)
        ))
    }
}

pub fn run_write_bmap(
    image: &std::path::Path,
    bmap_path: &std::path::Path,
    disk: &DiskInfo,
    opts: &JobOpts,
    prog: &Shared,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let xml = std::fs::read_to_string(bmap_path)?;
    let bm = bmap::parse(&xml)?;
    let src = Source::open(image)?;
    let mapped = bm.mapped_bytes();
    set_stage(
        prog,
        Stage::Starting,
        Some(mapped),
        src.comp_size,
        "Preparing device (unmounting volumes)…",
    );

    let mut dev = disks::open_disk(disk, true).map_err(disks::err_hint)?;
    if bm.image_size > dev.size {
        return Err(io::Error::other(format!(
            "image ({}) is larger than the device ({})",
            disks::human_size(bm.image_size),
            disks::human_size(dev.size)
        )));
    }

    let sector = dev.sector as usize;
    let comp_pos = src.comp_pos.clone();
    let mut reader = src.reader;
    let mut meter = SpeedMeter::new();

    set_stage(
        prog,
        Stage::Writing,
        Some(mapped),
        src.comp_size,
        "Writing mapped ranges…",
    );
    let stats = bmap::copy_ranges(
        &mut reader,
        &mut dev.file,
        &bm,
        sector,
        opts.buf_bytes,
        true,
        cancel,
        |done| tick(prog, done, comp_pos.load(Ordering::Relaxed), &mut meter),
    )?;
    dev.file.flush()?;
    dev.sync();

    if opts.verify {
        set_stage(
            prog,
            Stage::Verifying,
            Some(mapped),
            0,
            "Verifying mapped ranges…",
        );
        dev.prepare_verify();
        let mut vmeter = SpeedMeter::new();
        bmap::verify_ranges(&mut dev.file, &bm, sector, opts.buf_bytes, cancel, |done| {
            tick(prog, done, 0, &mut vmeter)
        })?;
    }

    let eject_note = maybe_eject(&dev, disk, opts.eject);
    Ok(format!(
        "Done; {} mapped bytes written{}, {} skipped as unmapped{eject_note}",
        disks::human_size(stats.written),
        if opts.verify { " and verified" } else { "" },
        disks::human_size(stats.skipped),
    ))
}

pub fn run_read(
    out: &std::path::Path,
    disk: &DiskInfo,
    opts: &JobOpts,
    prog: &Shared,
    cancel: &AtomicBool,
) -> io::Result<String> {
    set_stage(prog, Stage::Starting, None, 0, "Preparing device…");
    let mut dev = disks::open_disk(disk, false).map_err(disks::err_hint)?;
    let total = dev.size;
    let mut sink = Sink::create(out, opts.compress, Some(total))?;
    let mut buf = AlignedBuf::new(opts.buf_bytes);
    let mut meter = SpeedMeter::new();
    let mut done: u64 = 0;

    set_stage(prog, Stage::Reading, Some(total), 0, "Reading device…");
    while done < total {
        if cancel.load(Ordering::Relaxed) {
            drop(sink);
            let _ = std::fs::remove_file(out);
            return Err(cancelled());
        }
        let n = ((total - done).min(opts.buf_bytes as u64)) as usize;
        dev.file.read_exact(&mut buf.as_mut_slice()[..n])?;
        sink.write_all(&buf.as_slice()[..n])?;
        done += n as u64;
        tick(prog, done, 0, &mut meter);
    }
    sink.finish()?;
    let out_size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    let eject_note = maybe_eject(&dev, disk, opts.eject);
    Ok(format!(
        "Backup saved: {} ({} read, file size {}){eject_note}",
        out.display(),
        disks::human_size(total),
        disks::human_size(out_size)
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn stage_terminal() {
        for s in [Stage::Done, Stage::Error, Stage::Cancelled] {
            assert!(s.is_terminal());
        }
        for s in [
            Stage::Idle,
            Stage::Starting,
            Stage::Writing,
            Stage::Verifying,
            Stage::Reading,
        ] {
            assert!(!s.is_terminal());
        }
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
    fn cancelled_is_interrupted() {
        assert_eq!(cancelled().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn decoder_chunks_then_eof() {
        let data = vec![3u8; 10 * 1024];
        let (rx, pool) = spawn_decoder(Box::new(Cursor::new(data)), 4096);
        let mut lens = Vec::new();
        loop {
            let c = next_chunk(&rx).unwrap();
            if c.len == 0 {
                break;
            }
            if c.len == 2048 {
                assert_eq!(&c.buf.as_slice()[..8], &[3u8; 8]);
            }
            lens.push(c.len);
            // decoder needs a free buffer to detect EOF
            let _ = pool.try_send(c.buf);
        }
        assert_eq!(lens, vec![4096, 4096, 2048]);
        assert!(next_chunk(&rx).is_err());
    }

    #[test]
    fn decoder_propagates_error() {
        struct Bad;
        impl Read for Bad {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }
        let (rx, _pool) = spawn_decoder(Box::new(Bad), 4096);
        let e = match next_chunk(&rx) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(e.to_string().contains("boom"));
    }

    #[test]
    fn decoder_pool_recycles() {
        let data: Vec<u8> = (0..20480u32).map(|i| i as u8).collect();
        let (rx, pool) = spawn_decoder(Box::new(Cursor::new(data.clone())), 4096);
        let mut out = Vec::new();
        loop {
            let ch = next_chunk(&rx).unwrap();
            if ch.len == 0 {
                break;
            }
            out.extend_from_slice(&ch.buf.as_slice()[..ch.len]);
            let _ = pool.try_send(ch.buf);
        }
        assert_eq!(out, data);
    }

    #[test]
    fn progress_line_roundtrip() {
        let p = Progress {
            stage: Stage::Writing,
            done: 123,
            total: Some(456),
            comp_done: 7,
            comp_total: 8,
            speed: 1000.0,
            message: "hello | with pipe".into(),
        };
        let q = parse_progress(&format_progress(&p)).unwrap();
        assert_eq!(q.stage, Stage::Writing);
        assert_eq!(
            (q.done, q.total, q.comp_done, q.comp_total),
            (123, Some(456), 7, 8)
        );
        assert_eq!(q.speed, 1000.0);
        assert_eq!(q.message, "hello | with pipe");

        let p2 = Progress {
            total: None,
            ..Progress::idle()
        };
        assert_eq!(parse_progress(&format_progress(&p2)).unwrap().total, None);
        assert!(parse_progress("garbage").is_none());
        assert!(parse_progress("").is_none());
    }

    #[test]
    fn stage_codes_roundtrip() {
        for s in [
            Stage::Idle,
            Stage::Starting,
            Stage::Writing,
            Stage::Verifying,
            Stage::Reading,
            Stage::Done,
            Stage::Error,
            Stage::Cancelled,
        ] {
            assert_eq!(Stage::from_u8(s.as_u8()), s);
        }
        assert_eq!(Stage::from_u8(99), Stage::Idle);
    }

    #[test]
    fn speed_meter_starts_at_zero() {
        let mut m = SpeedMeter::new();
        assert_eq!(m.update(0), 0.0);
        let v = m.update(1024);
        assert!(v >= 0.0);
    }

    #[test]
    fn finish_maps_results_to_stages() {
        let ctx: Notify = Arc::new(|| {});
        let prog: Shared = Arc::new(Mutex::new(Progress::idle()));

        finish(Ok("all good".into()), &prog, &ctx);
        {
            let p = prog.lock().unwrap();
            assert_eq!(p.stage, Stage::Done);
            assert_eq!(p.message, "all good");
        }

        finish(Err(cancelled()), &prog, &ctx);
        assert_eq!(prog.lock().unwrap().stage, Stage::Cancelled);

        finish(Err(io::Error::other("bad")), &prog, &ctx);
        {
            let p = prog.lock().unwrap();
            assert_eq!(p.stage, Stage::Error);
            assert!(p.message.contains("bad"));
        }
    }

    #[test]
    fn set_stage_resets_counters() {
        let prog: Shared = Arc::new(Mutex::new(Progress {
            done: 99,
            comp_done: 99,
            speed: 9.9,
            ..Progress::idle()
        }));
        set_stage(&prog, Stage::Verifying, Some(10), 5, "v");
        let p = prog.lock().unwrap();
        assert_eq!(p.stage, Stage::Verifying);
        assert_eq!(
            (p.done, p.total, p.comp_done, p.comp_total),
            (0, Some(10), 0, 5)
        );
        assert_eq!(p.speed, 0.0);
        assert_eq!(p.message, "v");
    }

    #[test]
    fn tick_updates_progress() {
        let prog: Shared = Arc::new(Mutex::new(Progress::idle()));
        let mut m = SpeedMeter::new();
        tick(&prog, 123, 45, &mut m);
        let p = prog.lock().unwrap();
        assert_eq!((p.done, p.comp_done), (123, 45));
    }

    #[test]
    fn run_write_fails_on_missing_image() {
        let prog: Shared = Arc::new(Mutex::new(Progress::idle()));
        let cancel = AtomicBool::new(false);
        let disk = DiskInfo {
            id: "nonexistent".into(),
            name: "fake".into(),
            size: 0,
            removable: true,
            bus: String::new(),
        };
        let opts = JobOpts {
            verify: false,
            buf_bytes: 4096,
            compress: false,
            force: true,
            eject: false,
        };
        let missing = std::path::PathBuf::from("Z:/definitely/not/here.img");
        assert!(run_write(&missing, &disk, &opts, &prog, &cancel).is_err());
        assert!(run_read(&missing, &disk, &opts, &prog, &cancel).is_err());
    }
}
