use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

use quick_xml::Reader as XmlReader;
use quick_xml::events::Event;
use sha2::{Digest, Sha256};

use crate::Error;
use crate::disks::AlignedBuf;
use crate::stream::fill;

/// Reason why a bmap file was rejected.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("range blocks ({counted}) disagree with MappedBlocksCount ({declared})")]
    CountMismatch { counted: u64, declared: u64 },
    #[error("missing ImageSize, BlockSize or BlocksCount")]
    MissingHeader,
    #[error("no ranges in BlockMap")]
    NoRanges,
    #[error("bad number \"{text}\"")]
    Number { text: String },
    #[error("bad range \"{text}\"")]
    Range { text: String },
    #[error("range past BlocksCount")]
    RangePastEnd,
    #[error("unsupported checksum type \"{kind}\" (only sha256)")]
    UnsupportedChecksum { kind: String },
    #[error("ranges are not sorted or overlap")]
    Unsorted,
    #[error("malformed XML")]
    Xml(#[source] quick_xml::Error),
}

/// Parsed block map.
///
/// Holding a `Bmap` guarantees that the header is complete and that the
/// ranges are sorted, disjoint and inside the image.
#[derive(Debug, Clone)]
pub struct Bmap {
    block_size: u64,
    image_size: u64,
    ranges: Vec<Range>,
}

/// Run of mapped blocks with inclusive block numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    chksum: Option<String>,
    end: u64,
    start: u64,
}

#[derive(Debug)]
pub struct CopyStats {
    pub skipped: u64,
    pub written: u64,
}

impl Bmap {
    /// Writes the mapped ranges of `src` to `dev`.
    ///
    /// With `check_hashes`, every range is hashed while it streams through
    /// and a mismatch is reported as [`Error::ChecksumMismatch`].
    #[expect(clippy::too_many_arguments)]
    pub fn copy_ranges<R, W>(
        &self,
        src: &mut R,
        dev: &mut W,
        sector: usize,
        buf_bytes: usize,
        check_hashes: bool,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64),
    ) -> Result<CopyStats, Error>
    where
        R: Read,
        W: Write + Seek,
    {
        let mut buf = AlignedBuf::new(buf_bytes);
        let mut pos = 0u64;
        let mut done = 0u64;

        for (index, r) in self.ranges.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let offset = self.range_offset(r);
            let len = self.range_len(r);

            let skip = offset - pos;
            if io::copy(&mut src.by_ref().take(skip), &mut io::sink())? != skip {
                return Err(Error::TruncatedImage);
            }
            pos = offset;

            dev.seek(SeekFrom::Start(offset))?;
            let mut hasher = check_hashes.then(Sha256::new);
            let mut remaining = len;
            while remaining > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = remaining.min(buf_bytes as u64) as usize;
                if fill(src, &mut buf.as_mut_slice()[..n])? != n {
                    return Err(Error::TruncatedImage);
                }
                if let Some(h) = &mut hasher {
                    h.update(&buf.as_slice()[..n]);
                }
                let padded = n.div_ceil(sector) * sector;
                buf.as_mut_slice()[n..padded].fill(0);
                dev.write_all(&buf.as_slice()[..padded])?;
                remaining -= n as u64;
                done += n as u64;
                on_progress(done);
            }
            pos += len;

            if let (Some(h), Some(want)) = (hasher, &r.chksum)
                && hex(&h.finalize()) != *want
            {
                return Err(Error::ChecksumMismatch {
                    index,
                    start: r.start,
                    end: r.end,
                });
            }
        }
        Ok(CopyStats {
            skipped: self.image_size - done,
            written: done,
        })
    }

    pub fn image_size(&self) -> u64 {
        self.image_size
    }

    pub fn mapped_bytes(&self) -> u64 {
        self.ranges.iter().map(|r| self.range_len(r)).sum()
    }

    /// Reads the ranges that carry a checksum back from `dev` and compares.
    pub fn verify_ranges<D>(
        &self,
        dev: &mut D,
        sector: usize,
        buf_bytes: usize,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64),
    ) -> Result<(), Error>
    where
        D: Read + Seek,
    {
        let mut buf = AlignedBuf::new(buf_bytes);
        let mut done = 0u64;
        for (index, r) in self.ranges.iter().enumerate() {
            let Some(want) = &r.chksum else { continue };
            dev.seek(SeekFrom::Start(self.range_offset(r)))?;
            let mut hasher = Sha256::new();
            let mut remaining = self.range_len(r);
            while remaining > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = remaining.min(buf_bytes as u64) as usize;
                let padded = n.div_ceil(sector) * sector;
                dev.read_exact(&mut buf.as_mut_slice()[..padded])?;
                hasher.update(&buf.as_slice()[..n]);
                remaining -= n as u64;
                done += n as u64;
                on_progress(done);
            }
            if hex(&hasher.finalize()) != *want {
                return Err(Error::ReadBackMismatch {
                    index,
                    start: r.start,
                    end: r.end,
                });
            }
        }
        Ok(())
    }

    fn range_offset(&self, r: &Range) -> u64 {
        r.start * self.block_size
    }

    // the last range may run past ImageSize
    fn range_len(&self, r: &Range) -> u64 {
        let off = self.range_offset(r);
        ((r.end - r.start + 1) * self.block_size).min(self.image_size.saturating_sub(off))
    }
}

impl FromStr for Bmap {
    type Err = ParseError;

    fn from_str(xml: &str) -> Result<Bmap, ParseError> {
        let mut reader = XmlReader::from_str(xml);
        let mut path: Vec<String> = Vec::new();
        let mut image_size = 0u64;
        let mut block_size = 0u64;
        let mut blocks_count = 0u64;
        let mut mapped_blocks = 0u64;
        let mut checksum_type: Option<String> = None;
        let mut ranges: Vec<Range> = Vec::new();
        let mut pending_chksum: Option<String> = None;

        loop {
            match reader.read_event().map_err(ParseError::Xml)? {
                Event::Start(e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if name == "Range" {
                        pending_chksum = None;
                        for a in e.attributes().flatten() {
                            let key = a.key.as_ref();
                            if key == b"chksum" || key == b"sha1" {
                                pending_chksum =
                                    Some(String::from_utf8_lossy(&a.value).trim().to_lowercase());
                            }
                        }
                    }
                    path.push(name);
                }
                Event::Text(t) => {
                    let text = t
                        .decode()
                        .map_err(|err| ParseError::Xml(quick_xml::Error::Encoding(err)))?;
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let parse_num = || {
                        text.parse().map_err(|_| ParseError::Number {
                            text: text.to_owned(),
                        })
                    };
                    match path.last().map(String::as_str) {
                        Some("ImageSize") => image_size = parse_num()?,
                        Some("BlockSize") => block_size = parse_num()?,
                        Some("BlocksCount") => blocks_count = parse_num()?,
                        Some("MappedBlocksCount") => mapped_blocks = parse_num()?,
                        Some("ChecksumType") => checksum_type = Some(text.to_lowercase()),
                        Some("Range") => ranges.push(Range::parse(text, pending_chksum.take())?),
                        _ => {}
                    }
                }
                Event::End(_) => {
                    path.pop();
                }
                Event::Eof => break,
                _ => {}
            }
        }

        if let Some(kind) = checksum_type
            && kind != "sha256"
        {
            return Err(ParseError::UnsupportedChecksum { kind });
        }
        if image_size == 0 || block_size == 0 || blocks_count == 0 {
            return Err(ParseError::MissingHeader);
        }
        if ranges.is_empty() {
            return Err(ParseError::NoRanges);
        }
        if ranges.windows(2).any(|w| w[1].start <= w[0].end) {
            return Err(ParseError::Unsorted);
        }
        if ranges.iter().any(|r| r.end >= blocks_count) {
            return Err(ParseError::RangePastEnd);
        }
        let counted: u64 = ranges.iter().map(|r| r.end - r.start + 1).sum();
        if mapped_blocks > 0 && counted != mapped_blocks {
            return Err(ParseError::CountMismatch {
                counted,
                declared: mapped_blocks,
            });
        }
        Ok(Bmap {
            block_size,
            image_size,
            ranges,
        })
    }
}

impl Range {
    // "a-b" or a single block number "a"
    fn parse(text: &str, chksum: Option<String>) -> Result<Range, ParseError> {
        let bad = || ParseError::Range {
            text: text.to_owned(),
        };
        let (start, end) = match text.split_once('-') {
            Some((a, b)) => (
                a.trim().parse().map_err(|_| bad())?,
                b.trim().parse().map_err(|_| bad())?,
            ),
            None => {
                let n = text.parse().map_err(|_| bad())?;
                (n, n)
            }
        };
        if end < start {
            return Err(bad());
        }
        Ok(Range { chksum, end, start })
    }
}

/// Default bmap location for an image: `foo.wic.xz` and `foo.wic` both map
/// to `foo.wic.bmap`.
///
/// ```
/// use std::path::Path;
///
/// let bmap = buildcast_core::bmap::sidecar_path(Path::new("foo.wic.xz"));
/// assert_eq!(bmap, Path::new("foo.wic.bmap"));
/// ```
pub fn sidecar_path(image: &Path) -> PathBuf {
    let mut base = image.to_path_buf();
    if let Some(ext) = base.extension().map(|e| e.to_string_lossy().to_lowercase())
        && ["gz", "xz", "zst", "bz2", "bzip2"].contains(&ext.as_str())
    {
        base.set_extension("");
    }
    let mut name = base.into_os_string();
    name.push(".bmap");
    name.into()
}

/// Reads and parses a bmap file.
pub fn load(path: &Path) -> Result<Bmap, Error> {
    Ok(std::fs::read_to_string(path)
        .map_err(Error::ReadBmap)?
        .parse()?)
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn sha(data: &[u8]) -> String {
        hex(&Sha256::digest(data))
    }

    fn fixture_image() -> Vec<u8> {
        (0..64 * 1024u32).map(|i| (i % 251) as u8).collect()
    }

    // 64 KiB image, 4 KiB blocks, 16 blocks, mapped: 0-1, 4, 10-15 (tail
    // clamps)
    fn fixture(image: &[u8], image_size: u64) -> String {
        let bs = 4096usize;
        let r = |a: usize, b: usize| {
            let end = (b + 1) * bs;
            sha(&image[a * bs..end.min(image_size as usize)])
        };
        format!(
            r#"<?xml version="1.0" ?>
<bmap version="2.0">
    <ImageSize> {image_size} </ImageSize>
    <BlockSize> 4096 </BlockSize>
    <BlocksCount> 16 </BlocksCount>
    <MappedBlocksCount> 9 </MappedBlocksCount>
    <ChecksumType> sha256 </ChecksumType>
    <BmapFileChecksum> 0000 </BmapFileChecksum>
    <BlockMap>
        <Range chksum="{c0}"> 0-1 </Range>
        <Range chksum="{c1}"> 4 </Range>
        <Range chksum="{c2}"> 10-15 </Range>
    </BlockMap>
</bmap>"#,
            c0 = r(0, 1),
            c1 = r(4, 4),
            c2 = r(10, 15),
        )
    }

    fn parse(xml: &str) -> Result<Bmap, ParseError> {
        xml.parse()
    }

    #[test]
    fn parse_fixture() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        assert_eq!(bm.image_size, 64 * 1024);
        assert_eq!(bm.block_size, 4096);
        assert_eq!(bm.ranges.len(), 3);
        assert_eq!((bm.ranges[0].start, bm.ranges[0].end), (0, 1));
        assert_eq!((bm.ranges[1].start, bm.ranges[1].end), (4, 4));
        assert_eq!((bm.ranges[2].start, bm.ranges[2].end), (10, 15));
        assert!(bm.ranges.iter().all(|r| r.chksum.is_some()));
        assert_eq!(bm.mapped_bytes(), 9 * 4096);
    }

    #[test]
    fn parse_clamps_tail() {
        let img = fixture_image();
        // image ends 1000 bytes into block 15
        let size = 15 * 4096 + 1000;
        let bm = parse(&fixture(&img, size as u64)).unwrap();
        let last = bm.ranges.last().unwrap();
        assert_eq!(bm.range_len(last), 5 * 4096 + 1000);
        assert_eq!(bm.mapped_bytes(), 3 * 4096 + 5 * 4096 + 1000);
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(matches!(
            parse("<bmap></bmap>"),
            Err(ParseError::MissingHeader)
        ));
        assert!(parse("not xml at all").is_err());
        let img = fixture_image();
        let good = fixture(&img, 64 * 1024);
        assert!(matches!(
            parse(&good.replace("sha256", "md5")),
            Err(ParseError::UnsupportedChecksum { .. })
        ));
        assert!(matches!(
            parse(&good.replace("0-1", "1-0")),
            Err(ParseError::Range { .. })
        ));
        assert!(matches!(
            parse(&good.replace("10-15", "10-99")),
            Err(ParseError::RangePastEnd)
        ));
        assert!(matches!(
            parse(&good.replace("> 4 <", "> 1 <")),
            Err(ParseError::Unsorted)
        ));
        assert!(matches!(
            parse(&good.replace("<MappedBlocksCount> 9", "<MappedBlocksCount> 7")),
            Err(ParseError::CountMismatch {
                counted: 9,
                declared: 7
            })
        ));
    }

    #[test]
    fn sidecar_paths() {
        assert_eq!(
            sidecar_path(Path::new("a/foo.wic.xz")),
            Path::new("a/foo.wic.bmap")
        );
        assert_eq!(
            sidecar_path(Path::new("foo.wic")),
            Path::new("foo.wic.bmap")
        );
        assert_eq!(
            sidecar_path(Path::new("foo.WIC.ZST")),
            Path::new("foo.WIC.bmap")
        );
    }

    fn run_copy(image_size: u64) {
        let img = &fixture_image()[..image_size as usize];
        let bm = parse(&fixture(&fixture_image(), image_size)).unwrap();
        // unmapped device regions must stay untouched
        let mut dev = Cursor::new(vec![0xFFu8; 64 * 1024]);
        let cancel = AtomicBool::new(false);
        let stats = bm
            .copy_ranges(
                &mut Cursor::new(img.to_vec()),
                &mut dev,
                512,
                4096,
                true,
                &cancel,
                |_| {},
            )
            .unwrap();
        assert_eq!(stats.written, bm.mapped_bytes());
        assert_eq!(stats.skipped, image_size - bm.mapped_bytes());

        let d = dev.into_inner();
        for r in &bm.ranges {
            let off = bm.range_offset(r) as usize;
            let len = bm.range_len(r) as usize;
            assert_eq!(&d[off..off + len], &img[off..off + len]);
        }
        // blocks 2-3 and 5-9 are unmapped
        assert!(d[2 * 4096..4 * 4096].iter().all(|&b| b == 0xFF));
        assert!(d[5 * 4096..10 * 4096].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn copy_writes_only_mapped_ranges() {
        run_copy(64 * 1024);
    }

    #[test]
    fn copy_handles_clamped_tail() {
        run_copy(15 * 4096 + 1000);
    }

    #[test]
    fn copy_detects_checksum_mismatch() {
        let img = fixture_image();
        let mut corrupted = img.clone();
        corrupted[4096 * 4 + 7] ^= 0xFF;
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(false);
        let err = bm
            .copy_ranges(
                &mut Cursor::new(corrupted),
                &mut Cursor::new(vec![0u8; 64 * 1024]),
                512,
                4096,
                true,
                &cancel,
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(err, Error::ChecksumMismatch { index: 1, .. }));
        assert!(err.is_mismatch());
    }

    #[test]
    fn copy_rejects_truncated_image() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(false);
        let err = bm
            .copy_ranges(
                &mut Cursor::new(img[..20 * 1024].to_vec()),
                &mut Cursor::new(vec![0u8; 64 * 1024]),
                512,
                4096,
                false,
                &cancel,
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(err, Error::TruncatedImage));
    }

    #[test]
    fn copy_stops_on_cancel() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(true);
        let err = bm
            .copy_ranges(
                &mut Cursor::new(img),
                &mut Cursor::new(vec![0u8; 64 * 1024]),
                512,
                4096,
                false,
                &cancel,
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn verify_ranges_roundtrip() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(false);
        let mut dev = Cursor::new(vec![0xFFu8; 64 * 1024]);
        bm.copy_ranges(
            &mut Cursor::new(img.clone()),
            &mut dev,
            512,
            4096,
            false,
            &cancel,
            |_| {},
        )
        .unwrap();
        bm.verify_ranges(&mut dev, 512, 4096, &cancel, |_| {})
            .unwrap();

        // corrupt one mapped byte on the device
        let mut d = dev.into_inner();
        d[4096 * 11 + 3] ^= 1;
        let err = bm
            .verify_ranges(&mut Cursor::new(d), 512, 4096, &cancel, |_| {})
            .unwrap_err();
        assert!(matches!(err, Error::ReadBackMismatch { index: 2, .. }));
    }
}
