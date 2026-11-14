//! The pohunek CLI version check of `validate` and `serve`.
//!
//! The engine reads the JSON answers of the pohunek CLI at `pohunek.bin`; the supported range
//! names the releases whose answers it reads as they are.

// Rust guideline compliant 2026-06-26

use std::fmt;

/// Oldest supported pohunek release; `scripts/setup` keeps the same `POHUNEK_MIN_VERSION`.
///
/// The `host command run` time limits of the configuration follow pohunek 0.41.0.
const SUPPORTED_FROM: Release = Release::new(0, 41, 0);

/// First pohunek release past the supported range: the next major release.
///
/// A minor release that changes the JSON fails each review as `pohunek_response_invalid`.
const SUPPORTED_BEFORE: Release = Release::new(1, 0, 0);

/// Text before the release number in the output of `pohunek --version`.
const VERSION_PREFIX: &str = "pohunek ";

/// A pohunek release number, `MAJOR.MINOR.PATCH`, ordered part by part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Release {
    /// Major part.
    major: u64,
    /// Minor part.
    minor: u64,
    /// Patch part.
    patch: u64,
}

impl Release {
    /// Returns the release `major.minor.patch`.
    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Release {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Why a version output is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The output is not one line `pohunek MAJOR.MINOR.PATCH`.
    Invalid,
    /// The release is outside the supported range.
    Unsupported(Release),
}

/// Returns the release that `output` names when it is in the supported range.
///
/// The output is `pohunek ` and three decimal parts joined by dots, with at most one final line
/// feed. A pre-release or build suffix, other text and a part above `u64::MAX` are invalid.
fn supported_release(output: &str) -> Result<Release, Refusal> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    let number = line.strip_prefix(VERSION_PREFIX).ok_or(Refusal::Invalid)?;
    let mut parts = number.split('.');
    let mut next = || -> Result<u64, Refusal> {
        let part = parts.next().ok_or(Refusal::Invalid)?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Refusal::Invalid);
        }
        part.parse::<u64>().map_err(|_| Refusal::Invalid)
    };
    let release = Release::new(next()?, next()?, next()?);
    if parts.next().is_some() {
        return Err(Refusal::Invalid);
    }
    if release < SUPPORTED_FROM || release >= SUPPORTED_BEFORE {
        return Err(Refusal::Unsupported(release));
    }
    Ok(release)
}

#[cfg(test)]
mod tests {
    use super::{Refusal, Release, supported_release};

    /// Each row is an output of `pohunek --version` and what the check makes of it (#9).
    #[test]
    fn pohunek_version_output_is_read_and_checked_against_the_range() {
        let invalid = Err(Refusal::Invalid);
        let cases: [(&str, Result<Release, Refusal>); 23] = [
            ("pohunek 0.41.0\n", Ok(Release::new(0, 41, 0))),
            ("pohunek 0.41.0", Ok(Release::new(0, 41, 0))),
            ("pohunek 0.41.7\n", Ok(Release::new(0, 41, 7))),
            ("pohunek 0.42.0\n", Ok(Release::new(0, 42, 0))),
            ("pohunek 0.42.19\n", Ok(Release::new(0, 42, 19))),
            ("pohunek 0.042.0\n", Ok(Release::new(0, 42, 0))),
            (
                "pohunek 0.40.1\n",
                Err(Refusal::Unsupported(Release::new(0, 40, 1))),
            ),
            (
                "pohunek 0.38.99\n",
                Err(Refusal::Unsupported(Release::new(0, 38, 99))),
            ),
            ("pohunek 0.43.0\n", Ok(Release::new(0, 43, 0))),
            ("pohunek 0.99.99\n", Ok(Release::new(0, 99, 99))),
            (
                "pohunek 1.0.0\n",
                Err(Refusal::Unsupported(Release::new(1, 0, 0))),
            ),
            ("pohunek 0.42.0-rc.1\n", invalid),
            ("pohunek 0.42.0+build\n", invalid),
            ("pohunek 0.42\n", invalid),
            ("pohunek 0.42.0.1\n", invalid),
            ("pohunek dev\n", invalid),
            ("pohunek 0.42.0 (abc1234)\n", invalid),
            ("pohunek 0.42.0\n\n", invalid),
            ("pohunek 0.42.0\r\n", invalid),
            ("pohunek  0.42.0\n", invalid),
            ("pohunek-cli 0.42.0\n", invalid),
            ("pohunek 0.18446744073709551616.0\n", invalid),
            ("", invalid),
        ];
        for (output, want) in cases {
            assert_eq!(supported_release(output), want, "output {output:?}");
        }
    }
}
