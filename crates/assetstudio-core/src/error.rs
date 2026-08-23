use std::error::Error as StdError;
use std::fmt;
use std::io;

/// Error returned by the native `AssetStudio` parser.
#[derive(Debug)]
pub enum Error {
    /// An underlying stream or filesystem operation failed.
    Io(io::Error),
    /// Input bytes do not satisfy the expected Unity format contract.
    InvalidData(String),
    /// The input is valid, but requires a feature that has not been migrated.
    Unsupported(String),
}

impl Error {
    #[must_use]
    pub fn invalid_data(message: impl Into<String>) -> Self {
        Self::InvalidData(message.into())
    }

    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::InvalidData(message) => formatter.write_str(message),
            Self::Unsupported(message) => write!(formatter, "unsupported: {message}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidData(_) | Self::Unsupported(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A bounded writer validating an in-memory payload has no external I/O to
/// fail. Preserve its diagnostic while classifying budget/length failures as
/// invalid caller/input data instead of a filesystem error.
pub(crate) fn output_validation<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| match error {
        Error::Io(error) => Error::invalid_data(error.to_string()),
        error => error,
    })
}

pub type Result<T> = std::result::Result<T, Error>;
