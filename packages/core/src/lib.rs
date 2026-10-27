//! Core of BuildCast: image decoding, disk access and the copy, verify and
//! backup jobs shared by the GUI and the CLI.
//!
//! The package has no optional features.

pub mod bmap;
pub mod disks;
#[cfg(windows)]
pub mod elevate;
mod error;
pub mod stream;
pub mod worker;

pub use self::error::{Error, Report};
