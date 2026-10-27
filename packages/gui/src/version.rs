use std::fmt;
use std::str::FromStr;

/// Release version as `major.minor.patch`.
///
/// A leading `v` and a pre-release or build suffix are accepted and ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("malformed version")]
pub(crate) struct ParseVersionError;

impl Version {
    pub(crate) fn current() -> Version {
        env!("CARGO_PKG_VERSION")
            .parse()
            .expect("package version is semver")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Version, ParseVersionError> {
        let mut it = s.trim().trim_start_matches('v').splitn(3, '.');
        let mut next = |all_digits: bool| {
            let part = it.next().ok_or(ParseVersionError)?;
            let digits = if all_digits {
                part
            } else {
                part.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or_default()
            };
            digits.parse().map_err(|_| ParseVersionError)
        };
        Ok(Version {
            major: next(true)?,
            minor: next(true)?,
            patch: next(false)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_orders() {
        assert!(v("v0.1.2") > v("0.1.1"));
        assert!(v("1.0.0") > v("0.9.9"));
        assert!(v("v0.2.0-rc1") > v("0.1.9"));
        assert_eq!(v("0.1.1"), v("v0.1.1"));
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert!("garbage".parse::<Version>().is_err());
        assert!("1.2".parse::<Version>().is_err());
    }
}
