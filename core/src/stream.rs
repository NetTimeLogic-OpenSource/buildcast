use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

impl Format {
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

pub struct CountingReader<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

pub struct Source {
    pub reader: Box<dyn Read + Send>,
    pub format: Format,
    pub comp_size: u64,
    pub uncomp_size: Option<u64>,
    pub comp_pos: Arc<AtomicU64>,
}

pub fn probe(path: &Path) -> io::Result<(Format, u64, Option<u64>)> {
    let s = Source::open(path)?;
    Ok((s.format, s.comp_size, s.uncomp_size))
}

fn detect(magic: &[u8]) -> Format {
    if magic.len() >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        Format::Gzip
    } else if magic.len() >= 3 && &magic[..3] == b"BZh" {
        Format::Bzip2
    } else if magic.len() >= 6 && &magic[..6] == b"\xfd7zXZ\x00" {
        Format::Xz
    } else if magic.len() >= 4 && &magic[..4] == b"\x28\xb5\x2f\xfd" {
        Format::Zstd
    } else if magic.len() >= 4 && &magic[..4] == b"PK\x03\x04" {
        Format::Zip
    } else if magic.len() >= 262 && &magic[257..262] == b"ustar" {
        Format::Tar
    } else {
        Format::Raw
    }
}

fn is_tar_name(path: &Path) -> bool {
    let n = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    n.contains(".tar.")
        || n.ends_with(".tar")
        || n.ends_with(".tgz")
        || n.ends_with(".txz")
        || n.ends_with(".tbz")
        || n.ends_with(".tbz2")
}

impl Source {
    pub fn open(path: &Path) -> io::Result<Source> {
        let mut f = File::open(path)?;
        let comp_size = f.metadata()?.len();
        let mut magic = [0u8; 512];
        let got = fill(&mut f, &mut magic)?;
        f.seek(SeekFrom::Start(0))?;
        let fmt = detect(&magic[..got]);

        if fmt == Format::Zip {
            return open_zip(f, comp_size);
        }

        // gzip stores the uncompressed size (mod 4 GiB) in the last 4 bytes
        let gz_isize = if fmt == Format::Gzip && comp_size >= 18 {
            let mut b = [0u8; 4];
            f.seek(SeekFrom::End(-4))?;
            f.read_exact(&mut b)?;
            f.seek(SeekFrom::Start(0))?;
            match u32::from_le_bytes(b) as u64 {
                0 => None,
                n => Some(n),
            }
        } else {
            None
        };

        let xz_size = if fmt == Format::Xz {
            let s = xz_uncompressed_size(&mut f, comp_size);
            f.seek(SeekFrom::Start(0))?;
            s
        } else {
            None
        };

        let count = Arc::new(AtomicU64::new(0));
        let cr = CountingReader {
            inner: f,
            count: count.clone(),
        };

        let (mut reader, mut uncomp): (Box<dyn Read + Send>, Option<u64>) = match fmt {
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

        if fmt == Format::Tar || (fmt != Format::Raw && is_tar_name(path)) {
            let (r, size) = tar_first_entry(reader)?;
            reader = r;
            uncomp = Some(size);
        }

        Ok(Source {
            reader,
            format: fmt,
            comp_size,
            uncomp_size: uncomp,
            comp_pos: count,
        })
    }
}

// returns < buf.len() only at EOF
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

/* ----------------------------- XZ ----------------------------- */

fn xz_varint(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = *b.get(*p)?;
        *p += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift >= 63 {
            return None;
        }
    }
}

// total uncompressed size from the stream index at EOF; None for anything
// unexpected (progress then falls back to the compressed-position ratio).
// Multi-stream files report only the last stream
fn xz_uncompressed_size(f: &mut File, len: u64) -> Option<u64> {
    if len < 32 {
        return None;
    }
    let mut ft = [0u8; 12];
    f.seek(SeekFrom::End(-12)).ok()?;
    f.read_exact(&mut ft).ok()?;
    if &ft[10..12] != b"YZ" {
        return None;
    }
    let backward = u32::from_le_bytes(ft[4..8].try_into().ok()?) as u64;
    let index_size = (backward + 1) * 4;
    if index_size + 12 > len || index_size > 16 * 1024 * 1024 {
        return None;
    }
    f.seek(SeekFrom::End(-(12 + index_size as i64))).ok()?;
    let mut idx = vec![0u8; index_size as usize];
    f.read_exact(&mut idx).ok()?;
    if idx.first() != Some(&0) {
        return None;
    }
    let mut p = 1;
    let count = xz_varint(&idx, &mut p)?;
    if count > 1_000_000 {
        return None;
    }
    let mut total = 0u64;
    for _ in 0..count {
        let _unpadded = xz_varint(&idx, &mut p)?;
        total = total.checked_add(xz_varint(&idx, &mut p)?)?;
    }
    Some(total)
}

/* ----------------------------- TAR ----------------------------- */

fn octal(b: &[u8]) -> io::Result<u64> {
    let s: String = b
        .iter()
        .take_while(|&&c| c != 0 && c != b' ')
        .map(|&c| c as char)
        .collect();
    u64::from_str_radix(s.trim(), 8).map_err(|_| io::Error::other("bad tar size field"))
}

fn tar_first_entry(mut r: Box<dyn Read + Send>) -> io::Result<(Box<dyn Read + Send>, u64)> {
    for _ in 0..64 {
        let mut hdr = [0u8; 512];
        if fill(&mut r, &mut hdr)? < 512 || hdr.iter().all(|&b| b == 0) {
            break;
        }
        if &hdr[257..262] != b"ustar" {
            return Err(io::Error::other("not a valid tar archive"));
        }
        let size = octal(&hdr[124..136])?;
        let typeflag = hdr[156];
        if typeflag == 0 || typeflag == b'0' {
            return Ok((Box::new(r.take(size)), size));
        }
        let padded = size.div_ceil(512) * 512;
        io::copy(&mut r.by_ref().take(padded), &mut io::sink())?;
    }
    Err(io::Error::other("no regular file found in tar archive"))
}

/* ----------------------------- ZIP ----------------------------- */

fn u16le(b: &[u8]) -> u64 {
    u16::from_le_bytes([b[0], b[1]]) as u64
}
fn u32le(b: &[u8]) -> u64 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64
}
fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

/// Parse the zip64 extra field (id 0x0001) for missing 64-bit sizes.
/// Field order is: uncompressed size, then compressed size; each present
/// only when the corresponding 32-bit value is 0xFFFFFFFF.
fn parse_zip64_extra(extra: &[u8], usize_: &mut u64, csize: &mut u64) {
    let mut p = 0;
    while p + 4 <= extra.len() {
        let id = u16le(&extra[p..]);
        let sz = u16le(&extra[p + 2..]) as usize;
        let body = &extra[p + 4..(p + 4 + sz).min(extra.len())];
        if id == 0x0001 {
            let mut q = 0;
            if *usize_ == 0xFFFF_FFFF && q + 8 <= body.len() {
                *usize_ = u64le(&body[q..]);
                q += 8;
            }
            if *csize == 0xFFFF_FFFF && q + 8 <= body.len() {
                *csize = u64le(&body[q..]);
            }
            return;
        }
        p += 4 + sz;
    }
}

// used when the local header has a data descriptor and carries no sizes
fn central_sizes(f: &mut File) -> io::Result<(u64, u64)> {
    let len = f.metadata()?.len();
    let tail_len = len.min(66_000);
    f.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail)?;

    let eocd = tail
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .ok_or_else(|| io::Error::other("zip: end of central directory not found"))?;
    let mut cd_off = u32le(&tail[eocd + 16..]);
    if cd_off == 0xFFFF_FFFF {
        // zip64 locator sits 20 bytes before the EOCD
        if eocd < 20 || &tail[eocd - 20..eocd - 16] != b"PK\x06\x07" {
            return Err(io::Error::other("zip64 locator not found"));
        }
        let z64_eocd_off = u64le(&tail[eocd - 20 + 8..]);
        let mut z64 = [0u8; 56];
        f.seek(SeekFrom::Start(z64_eocd_off))?;
        f.read_exact(&mut z64)?;
        if &z64[..4] != b"PK\x06\x06" {
            return Err(io::Error::other("bad zip64 EOCD"));
        }
        cd_off = u64le(&z64[48..]);
    }

    let mut ch = [0u8; 46];
    f.seek(SeekFrom::Start(cd_off))?;
    f.read_exact(&mut ch)?;
    if &ch[..4] != b"PK\x01\x02" {
        return Err(io::Error::other("bad central directory header"));
    }
    let mut csize = u32le(&ch[20..]);
    let mut usize_ = u32le(&ch[24..]);
    let namelen = u16le(&ch[28..]) as usize;
    let extralen = u16le(&ch[30..]) as usize;
    if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
        let mut ne = vec![0u8; namelen + extralen];
        f.read_exact(&mut ne)?;
        parse_zip64_extra(&ne[namelen..], &mut usize_, &mut csize);
    }
    Ok((csize, usize_))
}

fn open_zip(mut f: File, file_size: u64) -> io::Result<Source> {
    let mut lh = [0u8; 30];
    f.read_exact(&mut lh)?;
    if &lh[..4] != b"PK\x03\x04" {
        return Err(io::Error::other("bad zip local header"));
    }
    let flags = u16le(&lh[6..]);
    let method = u16le(&lh[8..]);
    if flags & 0x1 != 0 {
        return Err(io::Error::other("encrypted ZIP archives are not supported"));
    }
    let mut csize = u32le(&lh[18..]);
    let mut usize_ = u32le(&lh[22..]);
    let namelen = u16le(&lh[26..]);
    let extralen = u16le(&lh[28..]);

    if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
        let mut ne = vec![0u8; (namelen + extralen) as usize];
        f.read_exact(&mut ne)?;
        parse_zip64_extra(&ne[namelen as usize..], &mut usize_, &mut csize);
    }
    if flags & 0x8 != 0 && csize == 0 {
        let (c, u) = central_sizes(&mut f)?;
        csize = c;
        usize_ = u;
    }
    if csize == 0 {
        return Err(io::Error::other("cannot determine zip entry size"));
    }

    let data_off = 30 + namelen + extralen;
    f.seek(SeekFrom::Start(data_off))?;
    let count = Arc::new(AtomicU64::new(0));
    let limited = CountingReader {
        inner: f,
        count: count.clone(),
    }
    .take(csize);

    let reader: Box<dyn Read + Send> = match method {
        0 => Box::new(limited),
        8 => Box::new(flate2::read::DeflateDecoder::new(limited)),
        m => {
            return Err(io::Error::other(format!(
                "unsupported zip compression method {m}"
            )))
        }
    };
    Ok(Source {
        reader,
        format: Format::Zip,
        comp_size: if csize > 0 { csize } else { file_size },
        uncomp_size: if usize_ > 0 { Some(usize_) } else { None },
        comp_pos: count,
    })
}

/* ----------------------------- Sink ----------------------------- */

pub enum Sink {
    Raw(File),
    Zstd(zstd::stream::write::Encoder<'static, File>),
}

impl Sink {
    pub fn create(path: &Path, compress: bool, pledged_size: Option<u64>) -> io::Result<Sink> {
        let f = File::create(path)?;
        if compress {
            let mut enc = zstd::stream::write::Encoder::new(f, 3)?;
            if let Some(n) = pledged_size {
                let _ = enc.set_pledged_src_size(Some(n));
            }
            Ok(Sink::Zstd(enc))
        } else {
            Ok(Sink::Raw(f))
        }
    }

    pub fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Sink::Raw(f) => f.write_all(buf),
            Sink::Zstd(e) => e.write_all(buf),
        }
    }

    pub fn finish(self) -> io::Result<()> {
        match self {
            Sink::Raw(f) => f.sync_all(),
            Sink::Zstd(e) => e.finish()?.sync_all(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    #[test]
    fn detect_formats() {
        assert_eq!(detect(&[0x1f, 0x8b, 0]), Format::Gzip);
        assert_eq!(detect(b"BZh9"), Format::Bzip2);
        assert_eq!(detect(b"\xfd7zXZ\x00rest"), Format::Xz);
        assert_eq!(detect(b"\x28\xb5\x2f\xfdrest"), Format::Zstd);
        assert_eq!(detect(b"PK\x03\x04rest"), Format::Zip);
        let mut tar = [0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(detect(&tar), Format::Tar);
        assert_eq!(detect(b"random data"), Format::Raw);
        assert_eq!(detect(b""), Format::Raw);
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
    fn tar_name_detection() {
        assert!(is_tar_name(&PathBuf::from("a.tar")));
        assert!(is_tar_name(&PathBuf::from("a.tar.gz")));
        assert!(is_tar_name(&PathBuf::from("A.TGZ")));
        assert!(is_tar_name(&PathBuf::from("a.txz")));
        assert!(is_tar_name(&PathBuf::from("a.tbz")));
        assert!(is_tar_name(&PathBuf::from("a.tbz2")));
        assert!(!is_tar_name(&PathBuf::from("a.img.xz")));
        assert!(!is_tar_name(&PathBuf::from("noext")));
    }

    #[test]
    fn counting_reader_counts() {
        let count = Arc::new(AtomicU64::new(0));
        let mut r = CountingReader {
            inner: Cursor::new(vec![7u8; 1000]),
            count: count.clone(),
        };
        let mut buf = [0u8; 300];
        let mut total = 0;
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        assert_eq!(total, 1000);
        assert_eq!(count.load(Ordering::Relaxed), 1000);
    }

    struct OneByte(Cursor<Vec<u8>>);
    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(1);
            self.0.read(&mut buf[..n])
        }
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

    #[test]
    fn octal_parsing() {
        assert_eq!(octal(b"0000644\0").unwrap(), 0o644);
        assert_eq!(octal(b"11 ").unwrap(), 9);
        assert!(octal(b"zzz").is_err());
        assert!(octal(b"").is_err());
    }

    #[test]
    fn varint_edges() {
        let mut p = 0;
        assert_eq!(xz_varint(&[0x7f], &mut p), Some(0x7f));
        let mut p = 0;
        assert_eq!(xz_varint(&[0x80, 0x01], &mut p), Some(128));
        let mut p = 0;
        assert_eq!(xz_varint(&[0x80], &mut p), None);
        let mut p = 0;
        assert_eq!(xz_varint(&[0xff; 10], &mut p), None);
        let mut p = 0;
        assert_eq!(xz_varint(&[], &mut p), None);
    }

    #[test]
    fn zip64_extra_parsing() {
        let mut extra = Vec::new();
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&123u64.to_le_bytes());
        extra.extend_from_slice(&456u64.to_le_bytes());
        let (mut u, mut c) = (0xFFFF_FFFFu64, 0xFFFF_FFFFu64);
        parse_zip64_extra(&extra, &mut u, &mut c);
        assert_eq!((u, c), (123, 456));

        let (mut u, mut c) = (5u64, 6u64);
        parse_zip64_extra(&[0x09, 0x00, 0x02, 0x00, 0xaa, 0xbb], &mut u, &mut c);
        assert_eq!((u, c), (5, 6));

        let (mut u, mut c) = (0xFFFF_FFFFu64, 7u64);
        let mut only_u = Vec::new();
        only_u.extend_from_slice(&1u16.to_le_bytes());
        only_u.extend_from_slice(&8u16.to_le_bytes());
        only_u.extend_from_slice(&99u64.to_le_bytes());
        parse_zip64_extra(&only_u, &mut u, &mut c);
        assert_eq!((u, c), (99, 7));
    }

    fn tmpfile(name: &str, content: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("buildcast-unit");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn xz_size_rejects_garbage() {
        let p = tmpfile("junk.bin", &[0xabu8; 100]);
        let mut f = File::open(&p).unwrap();
        assert_eq!(xz_uncompressed_size(&mut f, 100), None);

        let p = tmpfile("short.bin", &[1, 2, 3]);
        let mut f = File::open(&p).unwrap();
        assert_eq!(xz_uncompressed_size(&mut f, 3), None);
    }
}
