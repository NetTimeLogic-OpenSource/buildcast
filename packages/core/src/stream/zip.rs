use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use super::{CountingReader, Error, Format, Source};

const SIZE_IN_ZIP64: u64 = 0xFFFF_FFFF;

/// Opens the first entry of a ZIP archive.
pub(super) fn open(mut f: File, file_size: u64) -> Result<Source, Error> {
    let mut lh = [0u8; 30];
    f.read_exact(&mut lh)?;
    if &lh[..4] != b"PK\x03\x04" {
        return Err(Error::ZipLocalHeader);
    }
    let flags = u16le(&lh[6..]);
    let method = u16le(&lh[8..]);
    if flags & 0x1 != 0 {
        return Err(Error::ZipEncrypted);
    }
    let mut csize = u32le(&lh[18..]);
    let mut usize_ = u32le(&lh[22..]);
    let namelen = u16le(&lh[26..]);
    let extralen = u16le(&lh[28..]);

    if csize == SIZE_IN_ZIP64 || usize_ == SIZE_IN_ZIP64 {
        let mut ne = vec![0u8; (namelen + extralen) as usize];
        f.read_exact(&mut ne)?;
        parse_zip64_extra(&ne[namelen as usize..], &mut usize_, &mut csize);
    }
    if flags & 0x8 != 0 && csize == 0 {
        (csize, usize_) = central_sizes(&mut f)?;
    }
    if csize == 0 {
        return Err(Error::ZipUnknownSize);
    }

    f.seek(SeekFrom::Start(30 + namelen + extralen))?;
    let count = Arc::new(AtomicU64::new(0));
    let limited = CountingReader::new(f, count.clone()).take(csize);

    let reader: Box<dyn Read + Send> = match method {
        0 => Box::new(limited),
        8 => Box::new(flate2::read::DeflateDecoder::new(limited)),
        method => return Err(Error::ZipMethod { method }),
    };
    Ok(Source {
        comp_pos: count,
        comp_size: if csize > 0 { csize } else { file_size },
        format: Format::Zip,
        reader,
        uncomp_size: Some(usize_).filter(|&n| n > 0),
    })
}

// used when the local header has a data descriptor and carries no sizes
fn central_sizes(f: &mut File) -> Result<(u64, u64), Error> {
    let len = f.metadata()?.len();
    let tail_len = len.min(66_000);
    f.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail)?;

    let eocd = tail
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .ok_or(Error::ZipEndOfCentralDirectory)?;
    let mut cd_off = u32le(&tail[eocd + 16..]);
    if cd_off == SIZE_IN_ZIP64 {
        // the zip64 locator sits 20 bytes before the EOCD
        if eocd < 20 || &tail[eocd - 20..eocd - 16] != b"PK\x06\x07" {
            return Err(Error::Zip64Locator);
        }
        let z64_eocd_off = u64le(&tail[eocd - 20 + 8..]);
        let mut z64 = [0u8; 56];
        f.seek(SeekFrom::Start(z64_eocd_off))?;
        f.read_exact(&mut z64)?;
        if &z64[..4] != b"PK\x06\x06" {
            return Err(Error::Zip64EndOfCentralDirectory);
        }
        cd_off = u64le(&z64[48..]);
    }

    let mut ch = [0u8; 46];
    f.seek(SeekFrom::Start(cd_off))?;
    f.read_exact(&mut ch)?;
    if &ch[..4] != b"PK\x01\x02" {
        return Err(Error::ZipCentralHeader);
    }
    let mut csize = u32le(&ch[20..]);
    let mut usize_ = u32le(&ch[24..]);
    let namelen = u16le(&ch[28..]) as usize;
    let extralen = u16le(&ch[30..]) as usize;
    if csize == SIZE_IN_ZIP64 || usize_ == SIZE_IN_ZIP64 {
        let mut ne = vec![0u8; namelen + extralen];
        f.read_exact(&mut ne)?;
        parse_zip64_extra(&ne[namelen..], &mut usize_, &mut csize);
    }
    Ok((csize, usize_))
}

/// Reads missing 64-bit sizes from the zip64 extra field (id 0x0001).
///
/// The field holds the uncompressed size, then the compressed size. Each is
/// present only when its 32-bit counterpart is 0xFFFFFFFF.
fn parse_zip64_extra(extra: &[u8], usize_: &mut u64, csize: &mut u64) {
    let mut p = 0;
    while p + 4 <= extra.len() {
        let id = u16le(&extra[p..]);
        let sz = u16le(&extra[p + 2..]) as usize;
        let body = &extra[p + 4..(p + 4 + sz).min(extra.len())];
        if id == 0x0001 {
            let mut q = 0;
            if *usize_ == SIZE_IN_ZIP64 && q + 8 <= body.len() {
                *usize_ = u64le(&body[q..]);
                q += 8;
            }
            if *csize == SIZE_IN_ZIP64 && q + 8 <= body.len() {
                *csize = u64le(&body[q..]);
            }
            return;
        }
        p += 4 + sz;
    }
}

fn u16le(b: &[u8]) -> u64 {
    u64::from(u16::from_le_bytes([b[0], b[1]]))
}

fn u32le(b: &[u8]) -> u64 {
    u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip64_extra_parsing() {
        let mut extra = Vec::new();
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&123u64.to_le_bytes());
        extra.extend_from_slice(&456u64.to_le_bytes());
        let (mut u, mut c) = (SIZE_IN_ZIP64, SIZE_IN_ZIP64);
        parse_zip64_extra(&extra, &mut u, &mut c);
        assert_eq!((u, c), (123, 456));

        let (mut u, mut c) = (5u64, 6u64);
        parse_zip64_extra(&[0x09, 0x00, 0x02, 0x00, 0xAA, 0xBB], &mut u, &mut c);
        assert_eq!((u, c), (5, 6));

        let (mut u, mut c) = (SIZE_IN_ZIP64, 7u64);
        let mut only_u = Vec::new();
        only_u.extend_from_slice(&1u16.to_le_bytes());
        only_u.extend_from_slice(&8u16.to_le_bytes());
        only_u.extend_from_slice(&99u64.to_le_bytes());
        parse_zip64_extra(&only_u, &mut u, &mut c);
        assert_eq!((u, c), (99, 7));
    }
}
