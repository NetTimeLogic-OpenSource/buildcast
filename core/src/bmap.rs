use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use sha2::{Digest, Sha256};

use crate::disks::AlignedBuf;
use crate::stream::fill;

#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    // inclusive block numbers
    pub start: u64,
    pub end: u64,
    pub chksum: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Bmap {
    pub image_size: u64,
    pub block_size: u64,
    pub blocks_count: u64,
    pub mapped_blocks: u64,
    pub ranges: Vec<Range>,
}

impl Bmap {
    pub fn mapped_bytes(&self) -> u64 {
        self.ranges.iter().map(|r| self.range_len(r)).sum()
    }

    pub fn range_offset(&self, r: &Range) -> u64 {
        r.start * self.block_size
    }

    // last range may run past ImageSize; clamp
    pub fn range_len(&self, r: &Range) -> u64 {
        let off = self.range_offset(r);
        ((r.end - r.start + 1) * self.block_size).min(self.image_size.saturating_sub(off))
    }
}

fn parse_range_text(s: &str) -> io::Result<(u64, u64)> {
    let s = s.trim();
    let bad = || io::Error::other(format!("bmap: bad range \"{s}\""));
    if let Some((a, b)) = s.split_once('-') {
        let start = a.trim().parse().map_err(|_| bad())?;
        let end = b.trim().parse().map_err(|_| bad())?;
        if end < start {
            return Err(bad());
        }
        Ok((start, end))
    } else {
        let n = s.parse().map_err(|_| bad())?;
        Ok((n, n))
    }
}

pub fn parse(xml: &str) -> io::Result<Bmap> {
    let mut reader = XmlReader::from_str(xml);
    let mut path: Vec<String> = Vec::new();
    let mut image_size = 0u64;
    let mut block_size = 0u64;
    let mut blocks_count = 0u64;
    let mut mapped_blocks = 0u64;
    let mut checksum_type: Option<String> = None;
    let mut ranges: Vec<Range> = Vec::new();
    let mut pending_chksum: Option<String> = None;

    let err = |m: &str| io::Error::other(format!("bmap: {m}"));

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "Range" {
                    pending_chksum = None;
                    for a in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(a.key.as_ref()).to_string();
                        if key == "chksum" || key == "sha1" {
                            pending_chksum =
                                Some(String::from_utf8_lossy(&a.value).trim().to_lowercase());
                        }
                    }
                }
                path.push(name);
            }
            Ok(Event::Text(t)) => {
                let text = t.unescape().map_err(|e| err(&e.to_string()))?;
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let parse_num = || -> io::Result<u64> {
                    text.parse()
                        .map_err(|_| err(&format!("bad number \"{text}\"")))
                };
                match path.last().map(String::as_str) {
                    Some("ImageSize") => image_size = parse_num()?,
                    Some("BlockSize") => block_size = parse_num()?,
                    Some("BlocksCount") => blocks_count = parse_num()?,
                    Some("MappedBlocksCount") => mapped_blocks = parse_num()?,
                    Some("ChecksumType") => checksum_type = Some(text.to_lowercase()),
                    Some("Range") => {
                        let (start, end) = parse_range_text(text)?;
                        ranges.push(Range {
                            start,
                            end,
                            chksum: pending_chksum.take(),
                        });
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                path.pop();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(err(&e.to_string())),
        }
    }

    if let Some(t) = &checksum_type {
        if t != "sha256" {
            return Err(err(&format!(
                "unsupported checksum type \"{t}\" (only sha256)"
            )));
        }
    }
    if image_size == 0 || block_size == 0 || blocks_count == 0 {
        return Err(err("missing ImageSize/BlockSize/BlocksCount"));
    }
    if ranges.is_empty() {
        return Err(err("no ranges in BlockMap"));
    }
    let mut prev_end = None;
    for r in &ranges {
        if let Some(p) = prev_end {
            if r.start <= p {
                return Err(err("ranges not sorted or overlapping"));
            }
        }
        if r.end >= blocks_count {
            return Err(err("range past BlocksCount"));
        }
        prev_end = Some(r.end);
    }
    let bm = Bmap {
        image_size,
        block_size,
        blocks_count,
        mapped_blocks,
        ranges,
    };
    if mapped_blocks > 0 {
        let counted: u64 = bm.ranges.iter().map(|r| r.end - r.start + 1).sum();
        if counted != mapped_blocks {
            return Err(err(&format!(
                "range blocks ({counted}) disagree with MappedBlocksCount ({mapped_blocks})"
            )));
        }
    }
    Ok(bm)
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
pub struct CopyStats {
    pub written: u64,
    pub skipped: u64,
}

// checksum mismatches are reported as ErrorKind::InvalidData so callers can
// distinguish verification failure from device errors
#[allow(clippy::too_many_arguments)]
pub fn copy_ranges<R, W>(
    src: &mut R,
    dev: &mut W,
    bm: &Bmap,
    sector: usize,
    buf_bytes: usize,
    check_hashes: bool,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> io::Result<CopyStats>
where
    R: Read,
    W: Write + Seek,
{
    let mut buf = AlignedBuf::new(buf_bytes);
    let mut pos = 0u64;
    let mut done = 0u64;
    let truncated = || io::Error::other("image ended before the last mapped range");

    for (i, r) in bm.ranges.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "cancelled by user",
            ));
        }
        let offset = bm.range_offset(r);
        let len = bm.range_len(r);

        let skip = offset - pos;
        if io::copy(&mut src.by_ref().take(skip), &mut io::sink())? != skip {
            return Err(truncated());
        }
        pos = offset;

        dev.seek(SeekFrom::Start(offset))?;
        let mut hasher = check_hashes.then(Sha256::new);
        let mut remaining = len;
        while remaining > 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "cancelled by user",
                ));
            }
            let n = remaining.min(buf_bytes as u64) as usize;
            if fill(src, &mut buf.as_mut_slice()[..n])? != n {
                return Err(truncated());
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

        if let (Some(h), Some(want)) = (hasher, &r.chksum) {
            let got = hex(&h.finalize());
            if got != want.to_lowercase() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "checksum mismatch in range {i} (blocks {}-{}, offset {offset}): expected {want}, got {got}",
                        r.start, r.end
                    ),
                ));
            }
        }
    }
    Ok(CopyStats {
        written: done,
        skipped: bm.image_size - done,
    })
}

pub fn verify_ranges<D>(
    dev: &mut D,
    bm: &Bmap,
    sector: usize,
    buf_bytes: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> io::Result<()>
where
    D: Read + Seek,
{
    let mut buf = AlignedBuf::new(buf_bytes);
    let mut done = 0u64;
    for (i, r) in bm.ranges.iter().enumerate() {
        let Some(want) = &r.chksum else { continue };
        let offset = bm.range_offset(r);
        let len = bm.range_len(r);
        dev.seek(SeekFrom::Start(offset))?;
        let mut hasher = Sha256::new();
        let mut remaining = len;
        while remaining > 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "cancelled by user",
                ));
            }
            let n = remaining.min(buf_bytes as u64) as usize;
            let padded = n.div_ceil(sector) * sector;
            dev.read_exact(&mut buf.as_mut_slice()[..padded])?;
            hasher.update(&buf.as_slice()[..n]);
            remaining -= n as u64;
            done += n as u64;
            on_progress(done);
        }
        let got = hex(&hasher.finalize());
        if got != want.to_lowercase() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "read-back verification failed in range {i} (blocks {}-{}, offset {offset}): expected {want}, got {got}",
                    r.start, r.end
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    fn sha(data: &[u8]) -> String {
        hex(&Sha256::digest(data))
    }

    fn fixture_image() -> Vec<u8> {
        (0..64 * 1024u32).map(|i| (i % 251) as u8).collect()
    }

    // 64 KiB image, 4 KiB blocks, 16 blocks; mapped: 0-1, 4, 10-15 (tail clamps)
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

    #[test]
    fn parse_fixture() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        assert_eq!(bm.image_size, 64 * 1024);
        assert_eq!(bm.block_size, 4096);
        assert_eq!(bm.blocks_count, 16);
        assert_eq!(bm.mapped_blocks, 9);
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
        assert!(parse("<bmap></bmap>").is_err());
        assert!(parse("not xml at all").is_err());
        let img = fixture_image();
        let good = fixture(&img, 64 * 1024);
        assert!(parse(&good.replace("sha256", "md5")).is_err());
        assert!(parse(&good.replace("0-1", "1-0")).is_err());
        assert!(parse(&good.replace("10-15", "10-99")).is_err());
        assert!(parse(&good.replace("<MappedBlocksCount> 9", "<MappedBlocksCount> 7")).is_err());
    }

    fn run_copy(image_size: u64) {
        let img = &fixture_image()[..image_size as usize];
        let bm = parse(&fixture(&fixture_image(), image_size)).unwrap();
        // unmapped device regions must stay untouched
        let mut dev = Cursor::new(vec![0xFFu8; 64 * 1024]);
        let cancel = AtomicBool::new(false);
        let stats = copy_ranges(
            &mut Cursor::new(img.to_vec()),
            &mut dev,
            &bm,
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
        // block 2-3 and 5-9 unmapped
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
        corrupted[4096 * 4 + 7] ^= 0xff;
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(false);
        let err = copy_ranges(
            &mut Cursor::new(corrupted),
            &mut Cursor::new(vec![0u8; 64 * 1024]),
            &bm,
            512,
            4096,
            true,
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("range 1"));
    }

    #[test]
    fn copy_rejects_truncated_image() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(false);
        let err = copy_ranges(
            &mut Cursor::new(img[..20 * 1024].to_vec()),
            &mut Cursor::new(vec![0u8; 64 * 1024]),
            &bm,
            512,
            4096,
            false,
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("ended before"));
    }

    #[test]
    fn verify_ranges_roundtrip() {
        let img = fixture_image();
        let bm = parse(&fixture(&img, 64 * 1024)).unwrap();
        let cancel = AtomicBool::new(false);
        let mut dev = Cursor::new(vec![0xFFu8; 64 * 1024]);
        copy_ranges(
            &mut Cursor::new(img.clone()),
            &mut dev,
            &bm,
            512,
            4096,
            false,
            &cancel,
            |_| {},
        )
        .unwrap();
        verify_ranges(&mut dev, &bm, 512, 4096, &cancel, |_| {}).unwrap();

        // corrupt one mapped byte on the device
        let mut d = dev.into_inner();
        d[4096 * 11 + 3] ^= 1;
        let err = verify_ranges(&mut Cursor::new(d), &bm, 512, 4096, &cancel, |_| {}).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
