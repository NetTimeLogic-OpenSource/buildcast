mod sink;
mod tar;
mod xz;
mod zip;

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use self::sink::Sink;

/// Reason why an image cannot be opened.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("bad TAR size field")]
    TarSize,
    #[error("no regular file found in the TAR archive")]
    TarEmpty,
    #[error("not a valid TAR archive")]
    TarHeader,
    #[error("ZIP64 end of central directory locator not found")]
    Zip64Locator,
    #[error("bad ZIP64 end of central directory")]
    Zip64EndOfCentralDirectory,
    #[error("bad ZIP central directory header")]
    ZipCentralHeader,
    #[error("encrypted ZIP archives are not supported")]
    ZipEncrypted,
    #[error("ZIP end of central directory not found")]
    ZipEndOfCentralDirectory,
    #[error("bad ZIP local header")]
    ZipLocalHeader,
    #[error("unsupported ZIP compression method {method}")]
    ZipMethod { method: u64 },
    #[error("cannot determine the ZIP entry size")]
    ZipUnknownSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Raw,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Zip,
    Tar,
}

pub struct CountingReader<R> {
    count: Arc<AtomicU64>,
    inner: R,
}

/// Decoded image stream with its sizes.
pub struct Source {
    pub comp_pos: Arc<AtomicU64>,
    pub comp_size: u64,
    pub format: Format,
    pub reader: Box<dyn Read + Send>,
    pub uncomp_size: Option<u64>,
}

impl Format {
    fn detect(magic: &[u8]) -> Format {
        if magic.starts_with(&[0x1F, 0x8B]) {
            Format::Gzip
        } else if magic.starts_with(b"BZh") {
            Format::Bzip2
        } else if magic.starts_with(b"\xFD7zXZ\x00") {
            Format::Xz
        } else if magic.starts_with(b"\x28\xB5\x2F\xFD") {
            Format::Zstd
        } else if magic.starts_with(b"PK\x03\x04") {
            Format::Zip
        } else if magic.get(257..262) == Some(b"ustar") {
            Format::Tar
        } else {
            Format::Raw
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Raw => "raw image",
            Format::Gzip => "gzip compressed",
            Format::Bzip2 => "bzip2 compressed",
            Format::Xz => "xz compressed",
            Format::Zstd => "zstd compressed",
            Format::Zip => "ZIP archive",
            Format::Tar => "TAR archive",
        }
    }
}

impl<R> CountingReader<R> {
    fn new(inner: R, count: Arc<AtomicU64>) -> CountingReader<R> {
        CountingReader { count, inner }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Source {
    pub fn open(path: &Path) -> Result<Source, Error> {
        let mut f = File::open(path)?;
        let comp_size = f.metadata()?.len();
        let mut magic = [0u8; 512];
        let got = fill(&mut f, &mut magic)?;
        f.seek(SeekFrom::Start(0))?;
        let format = Format::detect(&magic[..got]);

        if format == Format::Zip {
            return zip::open(f, comp_size);
        }

        // gzip stores the uncompressed size (mod 4 GiB) in the last 4 bytes
        let gz_isize = if format == Format::Gzip && comp_size >= 18 {
            let mut b = [0u8; 4];
            f.seek(SeekFrom::End(-4))?;
            f.read_exact(&mut b)?;
            f.seek(SeekFrom::Start(0))?;
            Some(u64::from(u32::from_le_bytes(b))).filter(|&n| n > 0)
        } else {
            None
        };

        let xz_size = if format == Format::Xz {
            let s = xz::uncompressed_size(&mut f, comp_size);
            f.seek(SeekFrom::Start(0))?;
            s
        } else {
            None
        };

        let count = Arc::new(AtomicU64::new(0));
        let cr = CountingReader::new(f, count.clone());

        let (mut reader, mut uncomp_size): (Box<dyn Read + Send>, Option<u64>) = match format {
            Format::Raw | Format::Tar => (Box::new(cr), Some(comp_size)),
            Format::Gzip => (Box::new(flate2::read::MultiGzDecoder::new(cr)), gz_isize),
            Format::Bzip2 => (Box::new(bzip2::read::MultiBzDecoder::new(cr)), None),
            Format::Xz => (
                Box::new(liblzma::read::XzDecoder::new_multi_decoder(cr)),
                xz_size,
            ),
            Format::Zstd => (
                Box::new(zstd::stream::read::Decoder::new(cr)?),
                zstd::zstd_safe::get_frame_content_size(&magic[..got])
                    .ok()
                    .flatten(),
            ),
            Format::Zip => unreachable!(),
        };

        if format == Format::Tar || (format != Format::Raw && tar::has_tar_name(path)) {
            let (r, size) = tar::first_entry(reader)?;
            reader = r;
            uncomp_size = Some(size);
        }

        Ok(Source {
            comp_pos: count,
            comp_size,
            format,
            reader,
            uncomp_size,
        })
    }
}

/// Reads until `buf` is full or the reader is exhausted.
///
/// Returns less than `buf.len()` only at the end of the stream.
pub fn fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct OneByte(Cursor<Vec<u8>>);

    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(1);
            self.0.read(&mut buf[..n])
        }
    }

    #[test]
    fn detect_formats() {
        assert_eq!(Format::detect(&[0x1F, 0x8B, 0]), Format::Gzip);
        assert_eq!(Format::detect(b"BZh9"), Format::Bzip2);
        assert_eq!(Format::detect(b"\xFD7zXZ\x00rest"), Format::Xz);
        assert_eq!(Format::detect(b"\x28\xB5\x2F\xFDrest"), Format::Zstd);
        assert_eq!(Format::detect(b"PK\x03\x04rest"), Format::Zip);
        let mut tar = [0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(Format::detect(&tar), Format::Tar);
        assert_eq!(Format::detect(b"random data"), Format::Raw);
        assert_eq!(Format::detect(b""), Format::Raw);
    }

    #[test]
    fn format_names() {
        let all = [
            (Format::Raw, "raw image"),
            (Format::Gzip, "gzip compressed"),
            (Format::Bzip2, "bzip2 compressed"),
            (Format::Xz, "xz compressed"),
            (Format::Zstd, "zstd compressed"),
            (Format::Zip, "ZIP archive"),
            (Format::Tar, "TAR archive"),
        ];
        for (f, n) in all {
            assert_eq!(f.name(), n);
        }
    }

    #[test]
    fn counting_reader_counts() {
        let count = Arc::new(AtomicU64::new(0));
        let mut r = CountingReader::new(Cursor::new(vec![7u8; 1000]), count.clone());
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 1000);
        assert_eq!(count.load(Ordering::Relaxed), 1000);
    }

    #[test]
    fn fill_reads_across_short_reads() {
        let mut r = OneByte(Cursor::new(vec![9u8; 10]));
        let mut buf = [0u8; 8];
        assert_eq!(fill(&mut r, &mut buf).unwrap(), 8);
        assert_eq!(buf, [9u8; 8]);
        let mut rest = [0u8; 8];
        assert_eq!(fill(&mut r, &mut rest).unwrap(), 2);
    }
}
