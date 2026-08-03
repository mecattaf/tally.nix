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

    /// The standard message for a banned global, ending in what to do instead.
    ///
    /// Knowing a global is banned does not tell an author how to express the
    /// intent behind reaching for it, so every banned global names its
    /// replacement. The static lint and the hardened runtime share this text.
    #[must_use]
    pub fn banned_global(global: &str, location: SourceLocation) -> Self {
        Self::determinism(
            global,
            format!(
                "banned global {global} is unavailable in flow scripts because it would break \
                 replay; {}",
                banned_global_remedy(global)
            ),
            location,
        )
    }
}

/// What a supervisor should do about one error code, without reading prose.
///
/// `transient` says whether repeating the identical command can ever produce a
/// different answer; `resolution` names the bounded operation that clears it.
/// The pair exists because an unattended queue that cannot tell a permanent
/// identity refusal from a lost socket spends the night re-observing the
/// permanent one — the failure #251 is about.
const RECOVERY_FACTS: &[(&str, bool, &str)] = &[
    // Permanent: the run's recorded identity and the current inputs disagree.
    // Only an explicit generation rollover clears these.
    ("script-changed-mid-run", false, "supersede"),
    ("args-changed-mid-run", false, "supersede"),
    ("catalog-changed-mid-run", false, "supersede"),
    // Permanent, and the successor is already named in the same error.
    ("flow-run-superseded", false, "run-successor"),
    // Permanent, but a rollover does not clear it: the same ordinal re-derived
    // different work, which is a question about the script or configuration.
    ("replay-divergence", false, "investigate"),
    ("script-history-conflict", false, "investigate"),
    ("args-history-conflict", false, "investigate"),
    ("catalog-history-conflict", false, "investigate"),
    // Permanent until the operator repairs the durable lineage index.
    ("flow-lineage-unusable", false, "repair-lineage-ledger"),
    ("flow-lineage-conflict", false, "investigate"),
    // Transient: exactly the codes the flow client's own re-arm classification
    // already retries.
    ("daemon-unreachable", true, "retry"),
    ("daemon-timeout", true, "retry"),
    ("daemon-epoch-changed", true, "retry"),
];

/// Which identity-bearing input diverged, for the codes where that is fixed.
fn divergent_input(code: &str) -> Option<&'static str> {
    match code {
        "script-changed-mid-run" => Some("script"),
        "args-changed-mid-run" => Some("args"),
        "catalog-changed-mid-run" => Some("catalog"),
        _ => None,
    }
}

/// Attach the `transient` / `resolution` (and, where fixed, `divergentInput`)
/// facts for this error's code.
///
/// Applied wherever a classified error is constructed — the startup identity
/// pins, the mid-run daemon refusals, and the client's own translations — so one
/// wire code never has two different `details` contracts depending on where it
/// was raised. A code with no entry is left unstamped, and `errors.md` says that
/// absence means unclassified rather than transient.
#[must_use]
pub fn with_recovery_facts(error: FlowError) -> FlowError {
    let Some((_, transient, resolution)) = RECOVERY_FACTS
        .iter()
        .find(|(code, _, _)| *code == error.code)
    else {
        return error;
    };
    let error = error
        .detail("transient", *transient)
        .detail("resolution", *resolution);
    match divergent_input(&error.code) {
        Some(input) => error.detail("divergentInput", input),
        None => error,
    }
}

fn banned_global_remedy(global: &str) -> &'static str {
    match global {
        "Date" => "witness a clock reading in a node instead",
        "Math.random" => "derive the choice from witnessed input, or let members() pick, instead",
        "WeakRef" | "FinalizationRegistry" => {
            "collection timing is not reproducible — hold the value directly instead"
        }
        "eval" | "Function" => {
            "code built at run time is outside the script hash — write the branch out instead"
        }
        _ => "express the intent through a node result instead",
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
