use std::error::Error as StdError;
use std::{fmt, io};

use crate::disks::human_size;
use crate::{bmap, stream};

const PRIVILEGE_HINT: &str = if cfg!(windows) {
    "run as Administrator"
} else if cfg!(target_os = "macos") {
    "run with sudo"
} else {
    "add your user to the 'disk' group or run with sudo"
};

/// Failure of a disk job or one of its steps.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid bmap")]
    Bmap(#[from] bmap::ParseError),
    #[error("cancelled by user")]
    Cancelled,
    #[error(
        "checksum mismatch in bmap range {index} (blocks {start}-{end}), the image does not match \
         the bmap"
    )]
    ChecksumMismatch { index: usize, start: u64, end: u64 },
    #[error("the decoder thread stopped unexpectedly")]
    DecoderStopped,
    #[cfg(target_os = "macos")]
    #[error("diskutil {command} failed: {stderr}")]
    Diskutil { command: String, stderr: String },
    #[cfg(windows)]
    #[error("bad disk id {id}")]
    DiskId { id: String },
    #[error("cannot eject {id}")]
    Eject { id: String },
    #[cfg(windows)]
    #[error("elevation was declined or the helper could not start")]
    ElevationDeclined,
    #[error("cannot open the image")]
    Image(#[from] stream::Error),
    #[error(
        "the image ({}) is larger than the device ({})",
        human_size(*image),
        human_size(*device)
    )]
    ImageTooLarge { image: u64, device: u64 },
    #[error(transparent)]
    Io(io::Error),
    #[cfg(windows)]
    #[error("cannot lock a volume on the target device (files in use?)")]
    LockVolume,
    #[error("insufficient privileges, {PRIVILEGE_HINT}")]
    PermissionDenied(#[source] io::Error),
    #[cfg(target_os = "macos")]
    #[error("cannot parse diskutil output")]
    Plist(#[source] plist::Error),
    #[error("cannot read the bmap file")]
    ReadBmap(#[source] io::Error),
    #[error(
        "read-back verification failed in bmap range {index} (blocks {start}-{end}), the device \
         data does not match the image"
    )]
    ReadBackMismatch { index: usize, start: u64, end: u64 },
    #[error("the image ended before the last mapped range")]
    TruncatedImage,
    #[error("cannot unmount {mount}")]
    Unmount {
        mount: String,
        #[source]
        source: io::Error,
    },
    #[cfg(target_os = "macos")]
    #[error("cannot unmount /dev/{id} (files in use?)")]
    UnmountDisk { id: String },
    #[error(
        "verification failed at byte offset {offset}, the device data does not match the image"
    )]
    VerifyMismatch { offset: u64 },
}

impl Error {
    /// Whether the written data did not match the source.
    pub fn is_mismatch(&self) -> bool {
        matches!(
            self,
            Error::ChecksumMismatch { .. }
                | Error::ReadBackMismatch { .. }
                | Error::VerifyMismatch { .. }
        )
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        if err.kind() == io::ErrorKind::PermissionDenied {
            Error::PermissionDenied(err)
        } else {
            Error::Io(err)
        }
    }
}

/// Formats an error together with its chain of sources.
///
/// ```
/// use buildcast_core::{Error, Report};
///
/// let err = Error::from(std::io::Error::other("disk gone"));
/// assert_eq!(Report(&err).to_string(), "disk gone");
/// ```
pub struct Report<'a>(pub &'a dyn StdError);

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(err) = source {
            write!(f, ": {err}")?;
            source = err.source();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_adds_hint() {
        let err = Error::from(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(matches!(err, Error::PermissionDenied(_)));
        let hint = if cfg!(windows) {
            "Administrator"
        } else {
            "sudo"
        };
        assert!(err.to_string().contains(hint));

        let other = Error::from(io::Error::other("x"));
        assert_eq!(other.to_string(), "x");
    }

    #[test]
    fn report_includes_sources() {
        let err = Error::ReadBmap(io::Error::other("gone"));
        assert_eq!(Report(&err).to_string(), "cannot read the bmap file: gone");
    }
}
