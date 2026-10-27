use std::io::{self, Read};
use std::path::Path;

use super::{Error, fill};

const BLOCK: usize = 512;
// skip at most this many non-regular entries (long names, pax headers)
const MAX_SKIPPED: usize = 64;

/// Returns the first regular file in the archive and its size.
pub(super) fn first_entry(
    mut r: Box<dyn Read + Send>,
) -> Result<(Box<dyn Read + Send>, u64), Error> {
    for _ in 0..MAX_SKIPPED {
        let mut hdr = [0u8; BLOCK];
        if fill(&mut r, &mut hdr)? < BLOCK || hdr.iter().all(|&b| b == 0) {
            break;
        }
        if &hdr[257..262] != b"ustar" {
            return Err(Error::TarHeader);
        }
        let size = octal(&hdr[124..136])?;
        let typeflag = hdr[156];
        if typeflag == 0 || typeflag == b'0' {
            return Ok((Box::new(r.take(size)), size));
        }
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        io::copy(&mut r.by_ref().take(padded), &mut io::sink())?;
    }
    Err(Error::TarEmpty)
}

pub(super) fn has_tar_name(path: &Path) -> bool {
    let n = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    n.contains(".tar.")
        || [".tar", ".tgz", ".txz", ".tbz", ".tbz2"]
            .iter()
            .any(|ext| n.ends_with(ext))
}

fn octal(b: &[u8]) -> Result<u64, Error> {
    let digits: String = b
        .iter()
        .take_while(|&&c| c != 0 && c != b' ')
        .map(|&c| c as char)
        .collect();
    u64::from_str_radix(digits.trim(), 8).map_err(|_| Error::TarSize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tar_name_detection() {
        assert!(has_tar_name(Path::new("a.tar")));
        assert!(has_tar_name(Path::new("a.tar.gz")));
        assert!(has_tar_name(Path::new("A.TGZ")));
        assert!(has_tar_name(Path::new("a.txz")));
        assert!(has_tar_name(Path::new("a.tbz")));
        assert!(has_tar_name(Path::new("a.tbz2")));
        assert!(!has_tar_name(Path::new("a.img.xz")));
        assert!(!has_tar_name(Path::new("noext")));
    }

    #[test]
    fn octal_parsing() {
        assert_eq!(octal(b"0000644\0").unwrap(), 0o644);
        assert_eq!(octal(b"11 ").unwrap(), 9);
        assert!(octal(b"zzz").is_err());
        assert!(octal(b"").is_err());
    }
}
