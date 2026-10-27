use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// Total uncompressed size from the stream index at the end of the file.
///
/// Returns `None` for anything unexpected. Progress then falls back to the
/// compressed position. Multi-stream files report only the last stream.
pub(super) fn uncompressed_size(f: &mut File, len: u64) -> Option<u64> {
    if len < 32 {
        return None;
    }
    let mut footer = [0u8; 12];
    f.seek(SeekFrom::End(-12)).ok()?;
    f.read_exact(&mut footer).ok()?;
    if &footer[10..12] != b"YZ" {
        return None;
    }
    let backward = u64::from(u32::from_le_bytes(footer[4..8].try_into().ok()?));
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
    let count = varint(&idx, &mut p)?;
    if count > 1_000_000 {
        return None;
    }
    let mut total = 0u64;
    for _ in 0..count {
        let _unpadded = varint(&idx, &mut p)?;
        total = total.checked_add(varint(&idx, &mut p)?)?;
    }
    Some(total)
}

fn varint(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = *b.get(*p)?;
        *p += 1;
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift >= 63 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn tmpfile(name: &str, content: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("buildcast-unit");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn varint_edges() {
        let mut p = 0;
        assert_eq!(varint(&[0x7F], &mut p), Some(0x7F));
        let mut p = 0;
        assert_eq!(varint(&[0x80, 0x01], &mut p), Some(128));
        let mut p = 0;
        assert_eq!(varint(&[0x80], &mut p), None);
        let mut p = 0;
        assert_eq!(varint(&[0xFF; 10], &mut p), None);
        let mut p = 0;
        assert_eq!(varint(&[], &mut p), None);
    }

    #[test]
    fn size_rejects_garbage() {
        let p = tmpfile("junk.bin", &[0xABu8; 100]);
        let mut f = File::open(&p).unwrap();
        assert_eq!(uncompressed_size(&mut f, 100), None);

        let p = tmpfile("short.bin", &[1, 2, 3]);
        let mut f = File::open(&p).unwrap();
        assert_eq!(uncompressed_size(&mut f, 3), None);
    }
}
