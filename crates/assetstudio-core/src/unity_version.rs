use std::cmp::Ordering;
use std::error::Error as StdError;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

pub const BUILD_TYPE_ALPHA: &str = "a";
pub const BUILD_TYPE_BETA: &str = "b";
pub const BUILD_TYPE_FINAL: &str = "f";
pub const BUILD_TYPE_PATCH: &str = "p";
pub const BUILD_TYPE_TUANJIE: &str = "t";

/// A Unity editor/player version.
///
/// `AssetStudio`'s compatibility decisions compare only the major, minor, and
/// patch components. Build type and build number are retained for display and
/// special cases such as Tuanjie, but intentionally do not affect ordering.
#[derive(Debug, Clone)]
pub struct UnityVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: u32,
    pub build_type: Option<String>,
    pub full_version: String,
}

impl UnityVersion {
    #[must_use]
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        if (major, minor, patch) == (0, 0, 0) {
            return Self::default();
        }

        Self {
            major,
            minor,
            patch,
            build: 1,
            build_type: Some(BUILD_TYPE_FINAL.to_owned()),
            full_version: format!("{major}.{minor}.{patch}{BUILD_TYPE_FINAL}1"),
        }
    }

    #[must_use]
    pub const fn is_stripped(&self) -> bool {
        self.major == 0 && self.minor == 0 && self.patch == 0
    }

    #[must_use]
    pub fn is_alpha(&self) -> bool {
        self.build_type.as_deref() == Some(BUILD_TYPE_ALPHA)
    }

    #[must_use]
    pub fn is_beta(&self) -> bool {
        self.build_type.as_deref() == Some(BUILD_TYPE_BETA)
    }

    #[must_use]
    pub fn is_patch(&self) -> bool {
        self.build_type.as_deref() == Some(BUILD_TYPE_PATCH)
    }

    #[must_use]
    pub fn is_tuanjie(&self) -> bool {
        self.build_type.as_deref() == Some(BUILD_TYPE_TUANJIE) && self.components() >= (2022, 3, 2)
    }

    #[must_use]
    pub const fn components(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }

    /// Tests the half-open range `[lower, upper)` used throughout `AssetStudio`.
    #[must_use]
    pub fn is_in_range(&self, lower: (u32, u32, u32), upper: (u32, u32, u32)) -> bool {
        self.components() >= lower && self.components() < upper
    }
}

impl Default for UnityVersion {
    fn default() -> Self {
        Self {
            major: 0,
            minor: 0,
            patch: 0,
            build: 0,
            build_type: None,
            full_version: "0.0.0".to_owned(),
        }
    }
}

impl FromStr for UnityVersion {
    type Err = ParseUnityVersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(ParseUnityVersionError::new(input));
        }

        let number_runs = runs(input, |character| character.is_ascii_digit());
        if number_runs.len() < 3 {
            return Err(ParseUnityVersionError::new(input));
        }

        let parse_number = |index: usize| {
            number_runs[index]
                .parse::<u32>()
                .map_err(|_| ParseUnityVersionError::new(input))
        };
        let major = parse_number(0)?;
        let minor = parse_number(1)?;
        let patch = parse_number(2)?;
        let build = if number_runs.len() >= 4 {
            parse_number(3)?
        } else {
            0
        };

        // This mirrors the original Regex.Matches(version, @"\D+")[2]
        // behavior, including preservation of non-standard build markers.
        let non_digit_runs = runs(input, |character| !character.is_ascii_digit());
        let build_type = non_digit_runs.get(2).map(|value| (*value).to_owned());

        Ok(Self {
            major,
            minor,
            patch,
            build,
            build_type,
            full_version: input.to_owned(),
        })
    }
}

fn runs(input: &str, predicate: impl Fn(char) -> bool) -> Vec<&str> {
    let mut result = Vec::new();
    let mut run_start = None;

    for (index, character) in input.char_indices() {
        if predicate(character) {
            run_start.get_or_insert(index);
        } else if let Some(start) = run_start.take() {
            result.push(&input[start..index]);
        }
    }
    if let Some(start) = run_start {
        result.push(&input[start..]);
    }

    result
}

impl fmt::Display for UnityVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.full_version)
    }
}

impl PartialEq for UnityVersion {
    fn eq(&self, other: &Self) -> bool {
        self.components() == other.components()
    }
}

impl Eq for UnityVersion {}

impl PartialOrd for UnityVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for UnityVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.components().cmp(&other.components())
    }
}

impl Hash for UnityVersion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.components().hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseUnityVersionError {
    input: String,
}

impl ParseUnityVersionError {
    fn new(input: &str) -> Self {
        Self {
            input: input.to_owned(),
        }
    }
}

impl fmt::Display for ParseUnityVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to parse Unity version: \"{}\"",
            self.input
        )
    }
}

impl StdError for ParseUnityVersionError {}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::UnityVersion;

    #[test]
    fn parses_standard_version() {
        let version = UnityVersion::from_str("2022.3.62f1").unwrap();

        assert_eq!(version.components(), (2022, 3, 62));
        assert_eq!(version.build, 1);
        assert_eq!(version.build_type.as_deref(), Some("f"));
        assert_eq!(version.to_string(), "2022.3.62f1");
    }

    #[test]
    fn recognizes_tuanjie_versions() {
        assert!(UnityVersion::from_str("2022.3.2t1").unwrap().is_tuanjie());
        assert!(!UnityVersion::from_str("2022.3.1t1").unwrap().is_tuanjie());
        assert!(!UnityVersion::from_str("2022.3.2f1").unwrap().is_tuanjie());
    }

    #[test]
    fn compares_only_compatibility_components() {
        let final_build = UnityVersion::from_str("2021.3.5f1").unwrap();
        let patch_build = UnityVersion::from_str("2021.3.5p9").unwrap();

        assert_eq!(final_build, patch_build);
        assert!(final_build.is_in_range((2021, 3, 0), (2022, 1, 0)));
    }

    #[test]
    fn component_constructor_matches_original_defaults() {
        assert_eq!(UnityVersion::new(2019, 4, 0).to_string(), "2019.4.0f1");
        assert!(UnityVersion::new(0, 0, 0).is_stripped());
    }

    #[test]
    fn rejects_incomplete_versions() {
        assert!(UnityVersion::from_str("2022.3").is_err());
        assert!(UnityVersion::from_str("").is_err());
    }
}
