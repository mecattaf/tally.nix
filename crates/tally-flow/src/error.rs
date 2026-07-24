use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A one-based JavaScript source position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    #[must_use]
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

/// Stable, structured error surface shared by the checker and runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowError {
    pub name: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub details: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

impl FlowError {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            code: code.into(),
            message: message.into(),
            location: None,
            ordinal: None,
            details: Map::new(),
            stack: None,
        }
    }

    #[must_use]
    pub fn at(mut self, location: SourceLocation) -> Self {
        self.location = Some(location);
        self
    }

    #[must_use]
    pub fn at_if_missing(mut self, location: SourceLocation) -> Self {
        self.location.get_or_insert(location);
        self
    }

    #[must_use]
    pub fn with_ordinal(mut self, ordinal: u64) -> Self {
        self.ordinal = Some(ordinal);
        self
    }

    #[must_use]
    pub fn detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_stack(mut self, stack: impl Into<String>) -> Self {
        self.stack = Some(stack.into());
        self
    }

    #[must_use]
    pub fn report(&self) -> Value {
        serde_json::to_value(self).expect("FlowError serialization is infallible")
    }

    #[must_use]
    pub fn syntax(message: impl Into<String>, location: SourceLocation) -> Self {
        Self::new("FlowSyntaxError", "script-syntax", message).at(location)
    }

    #[must_use]
    pub fn determinism(global: &str, message: impl Into<String>, location: SourceLocation) -> Self {
        Self::new("FlowDeterminismError", "determinism-violation", message)
            .at(location)
            .detail("global", global)
    }
}

impl fmt::Display for FlowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} [{}]: {}", self.name, self.code, self.message)?;
        if let Some(location) = self.location {
            write!(
                formatter,
                " at line {}, column {}",
                location.line, location.column
            )?;
        }
        if let Some(ordinal) = self.ordinal {
            write!(formatter, " (ordinal {ordinal})")?;
        }
        Ok(())
    }
}

impl std::error::Error for FlowError {}
