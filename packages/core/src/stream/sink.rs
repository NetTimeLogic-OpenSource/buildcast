use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Backup output file, optionally zstd-compressed.
pub enum Sink {
    Raw(File),
    Zstd(zstd::stream::write::Encoder<'static, File>),
}

impl Sink {
    pub fn create(path: &Path, compress: bool, pledged_size: Option<u64>) -> io::Result<Sink> {
        let f = File::create(path)?;
        if !compress {
            return Ok(Sink::Raw(f));
        }
        let mut enc = zstd::stream::write::Encoder::new(f, 3)?;
        if let Some(n) = pledged_size {
            enc.set_pledged_src_size(Some(n))?;
        }
        Ok(Sink::Zstd(enc))
    }

    pub fn finish(self) -> io::Result<()> {
        match self {
            Sink::Raw(f) => f.sync_all(),
            Sink::Zstd(e) => e.finish()?.sync_all(),
        }
    }

    pub fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Sink::Raw(f) => f.write_all(buf),
            Sink::Zstd(e) => e.write_all(buf),
        }
    }
}
