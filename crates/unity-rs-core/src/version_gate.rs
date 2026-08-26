//! Shared semantics for per-class Unity version ceilings.
//!
//! Each version-gated reader classifies the effective Unity version once and
//! then parses. A standard-Unity version above the class's verified ceiling is
//! attempted with the newest known layout by default; only
//! [`SerializedFile::strict_unity_versions`](crate::serialized::SerializedFile)
//! restores the historical rejection. Stripped versions, versions below a
//! class floor, Tuanjie builds outside their verified range, and container
//! format gates are always rejected — leniency applies solely above the top of
//! the verified standard-Unity range.

use std::io;

use crate::unity_version::UnityVersion;
use crate::{Error, Result};

/// How a per-class version gate classified the effective Unity version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersionGateOutcome {
    /// Inside the class's verified range: parse and report errors unchanged.
    Verified,
    /// A standard-Unity version above the verified ceiling, admitted
    /// leniently: parse with the newest known layout, and reclassify a
    /// layout-shaped failure as `Unsupported` (see [`finish_lenient`]).
    AboveVerifiedRange,
}

/// Applies the lenient-parse error contract to a gated reader's result.
///
/// Inside the verified range the result passes through untouched. Above it, a
/// parse failure is presumptively a layout mismatch rather than corrupted
/// input, so `InvalidData` (including budget diagnostics reached through
/// untrustworthy above-ceiling counts), end-of-input `Io` errors, and nested
/// `Unsupported` errors are all reported as `Unsupported`, with the inner
/// diagnostic preserved verbatim. Genuine I/O failures other than
/// end-of-input keep their `Io` family.
pub(crate) fn finish_lenient<T>(
    outcome: VersionGateOutcome,
    class_name: &str,
    version: &UnityVersion,
    result: Result<T>,
) -> Result<T> {
    match outcome {
        VersionGateOutcome::Verified => result,
        VersionGateOutcome::AboveVerifiedRange => result.map_err(|error| match error {
            Error::InvalidData(message) | Error::Unsupported(message) => {
                Error::Unsupported(above_range_message(class_name, version, &message))
            }
            Error::Io(io_error) if io_error.kind() == io::ErrorKind::UnexpectedEof => {
                Error::Unsupported(above_range_message(
                    class_name,
                    version,
                    &io_error.to_string(),
                ))
            }
            other @ Error::Io(_) => other,
        }),
    }
}

fn above_range_message(class_name: &str, version: &UnityVersion, inner: &str) -> String {
    format!(
        "{class_name} for Unity {version} is above the verified range and was attempted with the newest known layout, which did not fit: {inner}"
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn version() -> UnityVersion {
        UnityVersion::from_str("6000.4.0f1").unwrap()
    }

    #[test]
    fn verified_outcome_passes_every_family_through() {
        let ok = finish_lenient(VersionGateOutcome::Verified, "Mesh", &version(), Ok(7));
        assert_eq!(ok.unwrap(), 7);

        for error in [
            Error::invalid_data("bad bytes"),
            Error::unsupported("missing support"),
            Error::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "short read")),
            Error::Io(io::Error::other("disk detached")),
        ] {
            let text = error.to_string();
            let unchanged =
                finish_lenient::<()>(VersionGateOutcome::Verified, "Mesh", &version(), Err(error))
                    .unwrap_err();
            assert_eq!(unchanged.to_string(), text);
        }
    }

    #[test]
    fn above_range_success_returns_unmarked() {
        let ok = finish_lenient(
            VersionGateOutcome::AboveVerifiedRange,
            "Mesh",
            &version(),
            Ok("value"),
        );
        assert_eq!(ok.unwrap(), "value");
    }

    #[test]
    fn above_range_reclassifies_layout_shaped_failures_as_unsupported() {
        for error in [
            Error::invalid_data("channel count 9 exceeding limit 8"),
            Error::unsupported("nested feature"),
            Error::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "short read")),
        ] {
            let inner = match &error {
                Error::InvalidData(message) | Error::Unsupported(message) => message.clone(),
                Error::Io(io_error) => io_error.to_string(),
            };
            let wrapped = finish_lenient::<()>(
                VersionGateOutcome::AboveVerifiedRange,
                "Mesh",
                &version(),
                Err(error),
            )
            .unwrap_err();
            let Error::Unsupported(message) = wrapped else {
                panic!("expected Unsupported, got {wrapped:?}");
            };
            assert!(message.contains("Mesh for Unity 6000.4.0f1"), "{message}");
            assert!(message.contains("above the verified range"), "{message}");
            assert!(message.contains(&inner), "{message}");
        }
    }

    #[test]
    fn above_range_keeps_genuine_io_failures_as_io() {
        let wrapped = finish_lenient::<()>(
            VersionGateOutcome::AboveVerifiedRange,
            "Mesh",
            &version(),
            Err(Error::Io(io::Error::other("disk detached"))),
        )
        .unwrap_err();
        assert!(matches!(wrapped, Error::Io(_)), "{wrapped:?}");
    }
}
