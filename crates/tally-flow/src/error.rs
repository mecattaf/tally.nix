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

/// The exit-20 family: run supersession and replay divergence.
///
/// These five codes share one wire contract because they share one operator
/// answer — the run's recorded identity and the work in front of it disagree,
/// so continuing would write a second history. `cli::flow::flow_error` maps
/// exactly this list to exit 20.
pub const SUPERSESSION_CODES: [&str; 5] = [
    "script-changed-mid-run",
    "args-changed-mid-run",
    "catalog-changed-mid-run",
    "flow-run-superseded",
    "replay-divergence",
];

/// Every member of the exit-20 `details` contract, in emission order.
///
/// All fourteen are present on every exit-20 error at every raising site, with
/// `null` where the code has nothing to say. A monitor therefore reads one
/// shape and never asks where the refusal came from — which is the whole point:
/// the shape used to depend on whether the runner's startup scan or a mid-run
/// admission raised it, so a driver had to special-case the site to find the
/// hash that moved.
pub const SUPERSESSION_DETAIL_FIELDS: [&str; 14] = [
    "flowRunId",
    "divergentInput",
    "recordedHash",
    "currentHash",
    "recordedLabel",
    "currentLabel",
    "taskUuid",
    "successorFlowRunId",
    "reason",
    "recordedAt",
    "kernelError",
    "remedy",
    "transient",
    "resolution",
];

/// Is this one of the five codes that carries the exit-20 details contract?
#[must_use]
pub fn is_supersession_code(code: &str) -> bool {
    SUPERSESSION_CODES.contains(&code)
}

/// The site-supplied members of the exit-20 `details` contract.
///
/// A raising site fills what it knows and leaves the rest; the derived members
/// (`divergentInput`, `remedy`, `transient`, `resolution`) are fixed by the code
/// and are never a caller's choice.
#[derive(Debug, Clone, Default)]
pub struct SupersessionDetails<'a> {
    /// The run whose recorded identity is in question. Every raising site in
    /// this tree knows it; blank — empty or whitespace-only, the same test
    /// `run_script` applies — renders as `null` rather than as a blank string,
    /// and suppresses the `remedy` derived from it. A flag-shaped identity
    /// (leading `-` after trim) is a run named badly rather than not named:
    /// it stays visible, but suppresses the `remedy` the same way, because a
    /// command interpolating it parses as flags and exits 2 in an operator's
    /// hands (#414).
    pub flow_run_id: &'a str,
    /// The hash the ledger recorded for the divergent input.
    pub recorded_hash: Option<&'a str>,
    /// The hash this runner computed for the same input, now.
    pub current_hash: Option<&'a str>,
    /// The node label the ledger recorded.
    pub recorded_label: Option<&'a str>,
    /// The node label this runner derived, now.
    pub current_label: Option<&'a str>,
    /// The durable row the refusal is about, where one is identified.
    pub task_uuid: Option<&'a str>,
    /// The run that replaces a retired one.
    pub successor_flow_run_id: Option<&'a str>,
    /// The recorded rollover reason, one of the closed `flow.supersede` set.
    pub reason: Option<&'a str>,
    /// When the rollover was recorded.
    pub recorded_at: Option<&'a str>,
    /// The daemon's own message, when the refusal was discovered through a
    /// kernel dedup-key conflict rather than by the runner's own comparison.
    pub kernel_error: Option<&'a str>,
}

/// Build the full exit-20 `details` map for one code.
#[must_use]
pub fn supersession_details(code: &str, fields: &SupersessionDetails<'_>) -> Map<String, Value> {
    let mut details = Map::new();
    let mut set = |key: &str, value: Option<&str>| {
        if let Some(value) = value {
            details.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    };
    set("flowRunId", Some(fields.flow_run_id));
    set("recordedHash", fields.recorded_hash);
    set("currentHash", fields.current_hash);
    set("recordedLabel", fields.recorded_label);
    set("currentLabel", fields.current_label);
    set("taskUuid", fields.task_uuid);
    set("successorFlowRunId", fields.successor_flow_run_id);
    set("reason", fields.reason);
    set("recordedAt", fields.recorded_at);
    set("kernelError", fields.kernel_error);
    complete_supersession_details(code, &mut details);
    details
}

/// One exit-20 error, carrying the whole contract.
///
/// Every code in the family is a `FlowReplayError`; the caller supplies the
/// location and, mid-run, the ordinal.
#[must_use]
pub fn supersession_error(
    code: &str,
    message: impl Into<String>,
    fields: &SupersessionDetails<'_>,
) -> FlowError {
    let mut error = FlowError::new("FlowReplayError", code, message);
    error.details = supersession_details(code, fields);
    error
}

/// Bring a partially populated exit-20 `details` map onto the contract.
///
/// Missing members become `null`, the derived members are (re)stated from the
/// code, and the whole map is re-emitted in `SUPERSESSION_DETAIL_FIELDS` order.
/// Anything a producer supplied beyond the contract is preserved after it: the
/// fourteen fields are a guaranteed floor, never a filter that silently drops a
/// diagnostic.
fn complete_supersession_details(code: &str, details: &mut Map<String, Value>) {
    // A producer that named no run — a foreign client, or a `details` payload
    // that was not an object — leaves this absent, and a blank string is the
    // same fact written differently. Both render as `null`: an error must not
    // say it does not know which run this is and hand over a command to fix
    // that run in the same breath. "Blank" is `trim().is_empty()` because that
    // is what `run_script` already means by it (`engine/mod.rs`); a
    // whitespace-only id passes a bare `is_empty()` and renders a command that
    // exits 2 in the operator's hands, which is the defect this guard exists to
    // prevent, one shape narrower.
    let flow_run_id = details
        .get("flowRunId")
        .and_then(Value::as_str)
        .filter(|flow_run_id| !flow_run_id.trim().is_empty())
        .map(ToOwned::to_owned);
    let mut ordered = Map::new();
    for field in SUPERSESSION_DETAIL_FIELDS {
        let value = match field {
            // A producer that named its run as something other than a string
            // named one badly, not not at all, and every other member of the
            // contract keeps whatever the producer sent. Dropping the value
            // here would make this the one member the completion filters rather
            // than floors, and would make the doc row's "null only when the
            // producer named no run" false. No `remedy` is derived from it —
            // that reads the string form, which this value does not have.
            "flowRunId" => match details.get("flowRunId") {
                Some(named) if !named.is_string() => named.clone(),
                _ => flow_run_id.clone().map_or(Value::Null, Value::String),
            },
            "divergentInput" => family_divergent_input(code)
                .map_or(Value::Null, |input| Value::String(input.to_owned())),
            // A remedy is a command an operator can type, so it exists only when
            // every argument of that command does. Only the three pins have one;
            // `flow-run-superseded` names its successor instead, and
            // `replay-divergence` resolves by investigation. A flag-shaped id is
            // present but cannot be typed: interpolating it puts a flag where
            // the command needs an operand, so the command exits 2 in an
            // operator's hands and none is rendered (#414).
            "remedy" => match (divergent_input(code), flow_run_id.as_deref()) {
                (Some(_), Some(flow_run_id)) if !is_flag_shaped(flow_run_id) => {
                    supersede_remedy(code, flow_run_id).into()
                }
                _ => Value::Null,
            },
            "transient" => {
                recovery_fact(code).map_or(Value::Null, |(transient, _)| transient.into())
            }
            "resolution" => recovery_fact(code)
                .map_or(Value::Null, |(_, resolution)| resolution.to_owned().into()),
            _ => details.get(field).cloned().unwrap_or(Value::Null),
        };
        ordered.insert(field.to_owned(), value);
    }
    for (key, value) in details.iter() {
        if !ordered.contains_key(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }
    *details = ordered;
}

/// The identity-bearing input that disagrees, across the whole exit-20 family.
///
/// `replay-divergence` is the payload member: the same ordinal re-derived
/// different canonical work. `flow-run-superseded` has none — nothing diverged,
/// the run was retired by decision.
fn family_divergent_input(code: &str) -> Option<&'static str> {
    match code {
        "replay-divergence" => Some("payload"),
        other => divergent_input(other),
    }
}

/// Which identity-bearing input diverged, for the three pins whose remedy is a
/// `flow.supersede` reason from the closed set.
fn divergent_input(code: &str) -> Option<&'static str> {
    match code {
        "script-changed-mid-run" => Some("script"),
        "args-changed-mid-run" => Some("args"),
        "catalog-changed-mid-run" => Some("catalog"),
        _ => None,
    }
}

fn recovery_fact(code: &str) -> Option<(bool, &'static str)> {
    RECOVERY_FACTS
        .iter()
        .find(|(known, _, _)| *known == code)
        .map(|(_, transient, resolution)| (*transient, *resolution))
}

/// The `tally flow supersede` invocation that clears one identity refusal.
///
/// `resolution: "supersede"` already told an unattended *supervisor* which
/// class of operation clears the code. It never told a *person* which command
/// to type, and after a binary advance the refusal does not explain itself: the
/// pin covers the bytes the runner serialized, so a run recorded by an earlier
/// tally is refused for an input the operator never touched. Naming the exact
/// command is what turns that into one documented step instead of a source
/// reading. The successor UUID is left as a placeholder deliberately — it must
/// be persisted before the call, because idempotency is keyed on the whole
/// triple.
#[must_use]
pub fn supersede_remedy(code: &str, flow_run_id: &str) -> String {
    let reason = match divergent_input(code) {
        Some(input) => format!("{input}-changed"),
        None => "operator".to_owned(),
    };
    format!(
        "tally flow supersede --flow-run-id {flow_run_id} --new-flow-run-id <FRESH-UUID> \
         --reason {reason}"
    )
}

/// Whether a run identity reads as a command flag rather than as a run id.
///
/// `trim().starts_with('-')` is the entire test — deliberately not UUID
/// validation. A producer can send anything, and the fourteen-member map
/// preserves whatever it sent; a pasted `--reason` names a run, badly, and
/// #401 item 3's ruling says a badly named run stays visible rather than
/// being dropped. But a remedy interpolates the id straight into argv, where
/// anything starting with a dash parses as a flag and the advertised command
/// exits 2 in the operator's hands, so no command may be derived from this
/// shape (#414).
fn is_flag_shaped(flow_run_id: &str) -> bool {
    flow_run_id.trim().starts_with('-')
}

/// The sentence appended to a mid-run identity refusal, naming both why a
/// byte-identical input can still be refused and the command that clears it.
///
/// The command is omitted when no run is named, on the same rule
/// `complete_supersession_details` applies to the `remedy` member: a refusal
/// that cannot say which run this is must not hand an operator an invocation
/// missing the argument that makes it run. A flag-shaped identity is the
/// sibling case (#414): a run *was* named, but in a form that parses as a
/// flag, so the sentence names the malformed identity instead of rendering a
/// command that cannot parse — and keeps the raw id visible, because a badly
/// named run is not the same fact as no run named. `message` is the field a
/// human actually reads, so the guards have to be here too — every call site
/// in this tree sits downstream of `run_script`'s `flow-run-id-missing`
/// refusal, but this function is public and a future one need not.
#[must_use]
pub fn identity_refusal_remedy_sentence(code: &str, flow_run_id: &str) -> String {
    let input = divergent_input(code).unwrap_or("input");
    let why = format!(
        "; the pin covers the exact bytes this runner hashed, so a run recorded by an earlier \
         tally can be refused for {input} it never changed"
    );
    if flow_run_id.trim().is_empty() {
        return format!("{why}.");
    }
    if is_flag_shaped(flow_run_id) {
        return format!(
            "{why}. The run was named, but its identity is malformed: it starts with a dash \
             and reads as a command flag, so no supersede command is rendered for it; the raw \
             identity stays visible in flowRunId."
        );
    }
    format!(
        "{why}. Retire the run and start a successor: {}",
        supersede_remedy(code, flow_run_id)
    )
}

/// Attach the `transient` / `resolution` (and, where fixed, `divergentInput`)
/// facts for this error's code.
///
/// Applied wherever a classified error is constructed — the startup identity
/// pins, the mid-run daemon refusals, and the client's own translations — so one
/// wire code never has two different `details` contracts depending on where it
/// was raised. A code with no entry is left unstamped, and `errors.md` says that
/// absence means unclassified rather than transient.
///
/// For the exit-20 family this also completes the whole `details` contract, so
/// even a refusal that reached the runner from somewhere other than tally's own
/// raising sites arrives on the documented shape.
#[must_use]
pub fn with_recovery_facts(mut error: FlowError) -> FlowError {
    if is_supersession_code(&error.code) {
        complete_supersession_details(&error.code, &mut error.details);
        return error;
    }
    let Some((transient, resolution)) = recovery_fact(&error.code) else {
        return error;
    };
    error
        .detail("transient", transient)
        .detail("resolution", resolution)
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
