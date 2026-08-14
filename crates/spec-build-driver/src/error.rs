use std::fmt;

pub(crate) type Result<T> = std::result::Result<T, DriverError>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct DriverError(pub(crate) String);

impl DriverError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for DriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}
