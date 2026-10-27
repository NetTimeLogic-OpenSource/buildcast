//! Round-trip tests for the input stream layer: format detection,
//! decompression, and first-entry extraction from ZIP and TAR.

use std::io::{Read, Write};
use std::path::PathBuf;

use buildcast_core::stream::{self, Format, Source};

fn payload() -> Vec<u8> {
    // 3 MiB of pseudo-random-ish data, bigger than typical codec buffers
    (0..3 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("buildcast-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn read_all(path: &std::path::Path) -> (Format, Vec<u8>, Option<u64>) {
    let mut s = Source::open(path).unwrap();
    let mut out = Vec::new();
    s.reader.read_to_end(&mut out).unwrap();
    (s.format, out, s.uncomp_size)
}

#[test]
fn raw_roundtrip() {
    let data = payload();
    let p = tmp("t.img");
    std::fs::write(&p, &data).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Raw);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

#[test]
fn gzip_roundtrip() {
    let data = payload();
    let p = tmp("t.img.gz");
    let f = std::fs::File::create(&p).unwrap();
    let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Gzip);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64)); // from gzip ISIZE trailer
}

#[test]
fn zstd_roundtrip() {
    let data = payload();
    let p = tmp("t.img.zst");
    let compressed = zstd::encode_all(&data[..], 3).unwrap();
    std::fs::write(&p, compressed).unwrap();
    let (fmt, out, _) = read_all(&p);
    assert_eq!(fmt, Format::Zstd);
    assert_eq!(out, data);
}

#[test]
fn xz_roundtrip() {
    let data = payload();
    let p = tmp("t.img.xz");
    let f = std::fs::File::create(&p).unwrap();
    let mut enc = liblzma::write::XzEncoder::new(f, 6);
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Xz);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64)); // from the xz stream index
}

#[test]
fn bzip2_roundtrip() {
    let data = payload();
    let p = tmp("t.img.bz2");
    let f = std::fs::File::create(&p).unwrap();
    let mut enc = bzip2::write::BzEncoder::new(f, bzip2::Compression::default());
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    let (fmt, out, _) = read_all(&p);
    assert_eq!(fmt, Format::Bzip2);
    assert_eq!(out, data);
}

/// Minimal stored (method 0) ZIP with one entry.
fn make_zip_stored(data: &[u8]) -> Vec<u8> {
    let name = b"disk.img";
    let crc = {
        let mut h = flate2::Crc::new();
        h.update(data);
        h.sum()
    };
    let mut v = Vec::new();
    // local file header
    v.extend_from_slice(b"PK\x03\x04");
    v.extend_from_slice(&20u16.to_le_bytes()); // version
    v.extend_from_slice(&0u16.to_le_bytes()); // flags
    v.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    v.extend_from_slice(&[0; 4]); // time+date
    v.extend_from_slice(&crc.to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes()); // csize
    v.extend_from_slice(&(data.len() as u32).to_le_bytes()); // usize
    v.extend_from_slice(&(name.len() as u16).to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // extra len
    v.extend_from_slice(name);
    v.extend_from_slice(data);
    // (no central directory needed: sizes are in the local header)
    v
}

#[test]
fn zip_stored_roundtrip() {
    let data = payload();
    let p = tmp("t.zip");
    std::fs::write(&p, make_zip_stored(&data)).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Zip);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

/// Minimal ustar TAR with a directory entry followed by one file.
fn make_tar(data: &[u8]) -> Vec<u8> {
    fn header(name: &str, size: u64, typeflag: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        let sz = format!("{size:011o}\0");
        h[124..136].copy_from_slice(sz.as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        // checksum: spaces while computing
        h[148..156].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|&b| b as u64).sum();
        let cs = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(cs.as_bytes());
        h
    }
    let mut v = Vec::new();
    v.extend_from_slice(&header("some/dir/", 0, b'5'));
    v.extend_from_slice(&header("some/dir/disk.img", data.len() as u64, b'0'));
    v.extend_from_slice(data);
    let pad = (512 - data.len() % 512) % 512;
    v.extend_from_slice(&vec![0u8; pad]);
    v.extend_from_slice(&[0u8; 1024]); // end blocks
    v
}

#[test]
fn tar_roundtrip() {
    let data = payload();
    let p = tmp("t.tar");
    std::fs::write(&p, make_tar(&data)).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Tar);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

#[test]
fn tar_gz_roundtrip() {
    let data = payload();
    let p = tmp("t.tar.gz");
    let f = std::fs::File::create(&p).unwrap();
    let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    enc.write_all(&make_tar(&data)).unwrap();
    enc.finish().unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Gzip);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

fn crc32(data: &[u8]) -> u32 {
    let mut h = flate2::Crc::new();
    h.update(data);
    h.sum()
}

fn local_header(
    flags: u16,
    method: u16,
    crc: u32,
    csize: u32,
    usize_: u32,
    name: &[u8],
    extra: &[u8],
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"PK\x03\x04");
    v.extend_from_slice(&20u16.to_le_bytes());
    v.extend_from_slice(&flags.to_le_bytes());
    v.extend_from_slice(&method.to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&crc.to_le_bytes());
    v.extend_from_slice(&csize.to_le_bytes());
    v.extend_from_slice(&usize_.to_le_bytes());
    v.extend_from_slice(&(name.len() as u16).to_le_bytes());
    v.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    v.extend_from_slice(name);
    v.extend_from_slice(extra);
    v
}

#[test]
fn zip_deflate_roundtrip() {
    let data = payload();
    let mut comp = Vec::new();
    {
        use flate2::write::DeflateEncoder;
        let mut enc = DeflateEncoder::new(&mut comp, flate2::Compression::default());
        enc.write_all(&data).unwrap();
        enc.finish().unwrap();
    }
    let mut v = local_header(
        0,
        8,
        crc32(&data),
        comp.len() as u32,
        data.len() as u32,
        b"disk.img",
        &[],
    );
    v.extend_from_slice(&comp);
    let p = tmp("t8.zip");
    std::fs::write(&p, v).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Zip);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

#[test]
fn zip64_local_header_roundtrip() {
    let data = payload();
    let mut extra = Vec::new();
    extra.extend_from_slice(&1u16.to_le_bytes());
    extra.extend_from_slice(&16u16.to_le_bytes());
    extra.extend_from_slice(&(data.len() as u64).to_le_bytes());
    extra.extend_from_slice(&(data.len() as u64).to_le_bytes());
    let mut v = local_header(
        0,
        0,
        crc32(&data),
        0xFFFF_FFFF,
        0xFFFF_FFFF,
        b"disk.img",
        &extra,
    );
    v.extend_from_slice(&data);
    let p = tmp("t64.zip");
    std::fs::write(&p, v).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Zip);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

#[test]
fn zip_data_descriptor_central_dir_fallback() {
    let data = payload();
    let lh = local_header(0x0008, 0, crc32(&data), 0, 0, b"disk.img", &[]);
    let mut v = lh.clone();
    v.extend_from_slice(&data);
    v.extend_from_slice(b"PK\x07\x08");
    v.extend_from_slice(&crc32(&data).to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    let cd_off = v.len() as u32;
    // central directory header
    v.extend_from_slice(b"PK\x01\x02");
    v.extend_from_slice(&20u16.to_le_bytes()); // version made by
    v.extend_from_slice(&20u16.to_le_bytes()); // version needed
    v.extend_from_slice(&0x0008u16.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // method stored
    v.extend_from_slice(&[0; 4]); // time+date
    v.extend_from_slice(&crc32(&data).to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes()); // name len
    v.extend_from_slice(&0u16.to_le_bytes()); // extra len
    v.extend_from_slice(&0u16.to_le_bytes()); // comment len
    v.extend_from_slice(&0u16.to_le_bytes()); // disk start
    v.extend_from_slice(&0u16.to_le_bytes()); // int attrs
    v.extend_from_slice(&[0; 4]); // ext attrs
    v.extend_from_slice(&[0; 4]); // local header offset
    v.extend_from_slice(b"disk.img");
    let cd_size = v.len() as u32 - cd_off;
    // EOCD
    v.extend_from_slice(b"PK\x05\x06");
    v.extend_from_slice(&[0; 4]); // disk numbers
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&cd_size.to_le_bytes());
    v.extend_from_slice(&cd_off.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    let p = tmp("tdd.zip");
    std::fs::write(&p, v).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Zip);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

#[test]
fn zip_encrypted_rejected() {
    let data = b"secret";
    let mut v = local_header(
        0x0001,
        0,
        crc32(data),
        data.len() as u32,
        data.len() as u32,
        b"a",
        &[],
    );
    v.extend_from_slice(data);
    let p = tmp("tenc.zip");
    std::fs::write(&p, v).unwrap();
    let err = match Source::open(&p) {
        Err(e) => e,
        Ok(_) => panic!("encrypted zip accepted"),
    };
    assert!(err.to_string().contains("encrypted"));
}

#[test]
fn zip_unsupported_method_rejected() {
    let data = b"x";
    let mut v = local_header(0, 99, 0, 1, 1, b"a", &[]);
    v.extend_from_slice(data);
    let p = tmp("tm99.zip");
    std::fs::write(&p, v).unwrap();
    let err = match Source::open(&p) {
        Err(e) => e,
        Ok(_) => panic!("bogus method accepted"),
    };
    assert!(err.to_string().contains("method"));
}

#[test]
fn tar_skips_long_name_entries() {
    let data = payload();
    fn header(name: &str, size: u64, typeflag: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let sz = format!("{size:011o}\0");
        h[124..136].copy_from_slice(sz.as_bytes());
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }
    let longname = b"very/long/path/to/disk.img\0";
    let mut v = Vec::new();
    v.extend_from_slice(&header("././@LongLink", longname.len() as u64, b'L'));
    v.extend_from_slice(longname);
    v.extend_from_slice(&vec![0u8; (512 - longname.len() % 512) % 512]);
    v.extend_from_slice(&header("disk.img", data.len() as u64, b'0'));
    v.extend_from_slice(&data);
    let p = tmp("tl.tar");
    std::fs::write(&p, v).unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Tar);
    assert_eq!(out, data);
    assert_eq!(size, Some(data.len() as u64));
}

#[test]
fn probe_reports_format_and_sizes() {
    let data = payload();
    let p = tmp("tp.img");
    std::fs::write(&p, &data).unwrap();
    let (fmt, comp, uncomp) = stream::probe(&p).unwrap();
    assert_eq!(fmt, Format::Raw);
    assert_eq!(comp, data.len() as u64);
    assert_eq!(uncomp, Some(data.len() as u64));
}

#[test]
fn empty_file_is_raw() {
    let p = tmp("tempty.img");
    std::fs::write(&p, b"").unwrap();
    let (fmt, out, size) = read_all(&p);
    assert_eq!(fmt, Format::Raw);
    assert!(out.is_empty());
    assert_eq!(size, Some(0));
}

#[test]
fn sink_raw_roundtrip() {
    let p = tmp("sink.raw");
    let mut s = stream::Sink::create(&p, false, None).unwrap();
    s.write_all(b"hello world").unwrap();
    s.finish().unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"hello world");
}

#[test]
fn sink_zstd_roundtrip() {
    let data = payload();
    let p = tmp("sink.zst");
    let mut s = stream::Sink::create(&p, true, Some(data.len() as u64)).unwrap();
    s.write_all(&data).unwrap();
    s.finish().unwrap();
    let out = zstd::decode_all(&std::fs::read(&p).unwrap()[..]).unwrap();
    assert_eq!(out, data);
}
