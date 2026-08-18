use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{schema_for, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub const GENERATED_SCHEMA_BEGIN: &str = "  // BEGIN RUST-GENERATED SPEC-BUILD ARGS SCHEMA";
pub const GENERATED_SCHEMA_END: &str = "  // END RUST-GENERATED SPEC-BUILD ARGS SCHEMA";

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct Nullable<T>(Option<T>);

impl<T: JsonSchema> JsonSchema for Nullable<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("{}::Nullable<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Option<T>>::json_schema(generator)
    }
}

#[allow(dead_code)]
#[derive(Debug)]
struct Optional<T>(Option<T>);

impl<T> Default for Optional<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<'de, T> Deserialize<'de> for Optional<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}

impl<T: JsonSchema> JsonSchema for Optional<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        format!("Optional_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("{}::Optional<{}>", module_path!(), T::schema_id()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::_schemars_private_non_optional_json_schema(generator)
    }

    fn _schemars_private_non_optional_json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::_schemars_private_non_optional_json_schema(generator)
    }

    fn _schemars_private_is_option() -> bool {
        true
    }
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
struct PositiveU64(#[schemars(range(min = 1))] u64);

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
struct NonEmptyString(#[schemars(length(min = 1))] String);

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
struct AgentModel(#[schemars(length(min = 1, max = 128))] String);

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(rename = "canonicalArgv")]
struct CanonicalArgv(
    #[schemars(
        length(min = 1),
        inner(length(min = 1), regex(pattern = r"^[^\u0000-\u001f\u007f]+$"))
    )]
    Vec<String>,
);

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename = "canonicalAgent")]
struct CanonicalAgent {
    #[schemars(length(min = 1))]
    adapter: String,
    argv: CanonicalArgv,
    priority: Priority,
    runtime_max_sec: Nullable<PositiveU64>,
    approval_policy: Nullable<NonEmptyString>,
    sandbox_policy: Nullable<NonEmptyString>,
    diagnosis_sandbox_policy: Nullable<NonEmptyString>,
    model: Nullable<AgentModel>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename = "canonicalSteward")]
struct CanonicalSteward {
    #[schemars(
        length(min = 1, max = 80),
        regex(pattern = r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
    )]
    adapter: String,
    argv: CanonicalArgv,
    #[schemars(
        extend("maxProperties" = 64, "propertyNames" = {"pattern": r"^[A-Za-z_][A-Za-z0-9_]*$"})
    )]
    env: BTreeMap<String, StewardEnvValue>,
    #[schemars(length(min = 1, max = 1024))]
    final_message_pattern: String,
    runtime_max_sec: Nullable<PositiveU64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
struct StewardEnvValue(#[schemars(length(min = 1, max = 4096))] String);

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
enum Priority {
    Interrupt,
    High,
    Medium,
    Low,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
enum MergeMethod {
    Merge,
    Squash,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(inline)]
enum LocalForge {
    #[serde(rename = "local")]
    Local,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields)]
#[schemars(rename = "canonicalGate")]
enum CanonicalGate {
    #[serde(rename = "command", rename_all = "camelCase")]
    Command {
        #[schemars(length(max = 80), regex(pattern = r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$"))]
        id: String,
        preflight_argv: CanonicalArgv,
        argv: CanonicalArgv,
        #[schemars(range(min = 1))]
        runtime_max_sec: u64,
    },
    #[serde(rename = "forbidPaths", rename_all = "camelCase")]
    ForbidPaths {
        #[schemars(length(max = 80), regex(pattern = r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$"))]
        id: String,
        #[schemars(
            length(min = 1, max = 128),
            inner(length(min = 1, max = 1024)),
            extend("uniqueItems" = true)
        )]
        forbid_paths: Vec<String>,
        #[schemars(range(min = 1))]
        runtime_max_sec: u64,
    },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct CanonicalCampaignRepository {
    #[schemars(regex(pattern = r"^/"))]
    checkout: String,
    #[schemars(length(min = 1))]
    base_branch: String,
    #[schemars(length(max = 80), regex(pattern = r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$"))]
    remote: String,
    forge: LocalForge,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct CampaignTaskReference {
    id: String,
    kind: CampaignTaskKind,
    issue: i64,
    dependencies: Vec<Value>,
    #[serde(default)]
    #[schemars(!default)]
    conflict_domains: Optional<Vec<Value>>,
    argv: Nullable<Vec<Value>>,
    runtime_max_sec: Nullable<i64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
enum CampaignTaskKind {
    Implementation,
    Checkpoint,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename = "canonicalCampaignManifest")]
struct CanonicalCampaignManifest {
    #[schemars(extend("const" = 1))]
    schema_version: u8,
    #[schemars(length(max = 80), regex(pattern = r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$"))]
    name: String,
    repository: CanonicalCampaignRepository,
    #[schemars(range(min = 1, max = 128))]
    max_tasks: u64,
    #[schemars(range(min = 1, max = 128))]
    max_parallel: u64,
    #[schemars(range(min = 1))]
    driver_runtime_max_sec: u64,
    runtime_max_sec: Nullable<PositiveU64>,
    #[schemars(
        length(max = 80),
        regex(
            pattern = r"^(?:[A-Za-z0-9_][A-Za-z0-9_.-]*|campaign/(?!\.{1,2}/)[A-Za-z0-9_.-]+/(?!\.{1,2}$)[A-Za-z0-9_.-]+)$"
        )
    )]
    pool: String,
    merge_method: MergeMethod,
    agent: CanonicalAgent,
    steward: Nullable<CanonicalSteward>,
    #[schemars(length(min = 1, max = 16))]
    gates: Vec<CanonicalGate>,
    #[schemars(length(min = 1, max = 128))]
    tasks: Vec<CampaignTaskReference>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct CanonicalCampaignTask {
    #[schemars(range(min = 1))]
    number: u64,
    #[schemars(length(min = 1, max = 300))]
    title: String,
    #[schemars(length(min = 1, max = 64_000))]
    body: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct CanonicalCampaignGraph {
    manifest: CanonicalCampaignManifest,
    #[schemars(length(min = 1, max = 128))]
    tasks: Vec<CanonicalCampaignTask>,
    #[schemars(regex(pattern = r"^sha256:[0-9a-f]{64}$"))]
    executable_digest: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct CampaignIssue {
    #[schemars(regex(pattern = r"^[1-9][0-9]*$"))]
    number: String,
    #[schemars(length(min = 1))]
    url: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct RepositoryConfig {
    #[schemars(regex(pattern = r"^/"))]
    checkout: String,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._/+-]+$"))]
    base_branch: String,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._-]+$"))]
    remote: String,
    forge: RepositoryForge,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(inline)]
enum RepositoryForge {
    Github,
    Local,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
enum Worklist {
    Path(#[schemars(length(min = 1))] String),
    Selector(WorklistSelector),
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct WorklistSelector {
    #[schemars(length(min = 1))]
    kind: String,
    #[schemars(regex(pattern = r"^sha256:[0-9a-f]{64}$"))]
    graph_digest: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct Continuation {
    #[schemars(
        length(min = 1, max = 64),
        inner(length(min = 1), regex(pattern = r"^[^\u0000-\u001f\u007f]+$"))
    )]
    argv: Vec<String>,
    #[schemars(
        length(min = 1, max = 8),
        inner(length(min = 1, max = 128)),
        extend("uniqueItems" = true)
    )]
    pool: Vec<String>,
    priority: Priority,
    #[schemars(range(min = 1))]
    runtime_max_sec: Option<u64>,
    #[schemars(regex(pattern = r"^/"))]
    events_dir: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct SteeringComment {
    #[schemars(range(min = 1))]
    id: u64,
    #[schemars(length(min = 1))]
    url: String,
    #[schemars(length(min = 1, max = 128))]
    author: String,
    #[schemars(length(max = 64_000))]
    body: String,
    #[schemars(length(min = 1))]
    created_at: String,
    #[schemars(length(min = 1))]
    updated_at: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
struct SteeringSource {
    #[schemars(extend("const" = 1))]
    schema_version: u8,
    kind: LocalJsonlKind,
    #[schemars(length(min = 1, max = 128))]
    registration_id: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = r"^[^\s/\\\u0000]+$"))]
    local_actor: String,
    #[schemars(regex(pattern = r"^/"))]
    log_path: String,
    #[schemars(regex(pattern = r"^/"))]
    lock_path: String,
    prepared_cursor: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
struct Sha256Identity(#[schemars(regex(pattern = r"^sha256:[0-9a-f]{64}$"))] String);

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(inline)]
enum LocalJsonlKind {
    #[serde(rename = "local-jsonl")]
    LocalJsonl,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(
    rename = "specBuildFlowArgs",
    extend(
        "oneOf" = [
            {
                "required": ["campaign", "repositories", "maxTasks", "maxParallel", "agent", "gates"],
                "properties": {"worklist": {"type": "string", "minLength": 1}}
            },
            {
                "required": ["campaignIdentity", "campaignGraph", "steering", "localActor", "steeringSource"],
                "properties": {"worklist": {"type": "object"}}
            }
        ],
        "allOf" = [
            {"if": {"required": ["taskSteering"]}, "then": {"required": ["localActor", "steeringSource"]}},
            {"if": {"required": ["localActor"]}, "then": {"required": ["steeringSource"]}},
            {"if": {"required": ["steeringSource"]}, "then": {"required": ["localActor"]}}
        ]
    )
)]
pub struct SpecBuildFlowArgs {
    #[serde(default)]
    #[schemars(
        !default,
        length(max = 80),
        regex(pattern = r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$")
    )]
    campaign: Optional<String>,
    #[serde(default)]
    #[schemars(!default, regex(pattern = r"^[0-9a-fA-F-]{36}$"))]
    campaign_identity: Optional<String>,
    #[serde(default)]
    #[schemars(!default)]
    campaign_graph: Optional<CanonicalCampaignGraph>,
    #[serde(default)]
    #[schemars(!default, extend("maxProperties" = 128))]
    task_input_hashes: Optional<BTreeMap<String, Sha256Identity>>,
    #[serde(default)]
    #[schemars(!default, extend("maxProperties" = 128))]
    task_completion_revisions: Optional<BTreeMap<String, Sha256Identity>>,
    armed_manifest: Option<CanonicalCampaignManifest>,
    #[serde(default)]
    #[schemars(!default)]
    allowed_actors: Optional<Vec<Value>>,
    #[serde(default)]
    #[schemars(!default)]
    capabilities: Optional<BTreeMap<String, Value>>,
    #[schemars(regex(pattern = r"^[^/ \t]+/[^/ \t]+$"))]
    repository: String,
    #[serde(default)]
    #[schemars(!default, regex(pattern = r"^[^/ \t]+/[^/ \t]+$"))]
    code_repository: Optional<String>,
    #[serde(default)]
    #[schemars(!default, regex(pattern = r"^[^/ \t]+/[^/ \t]+$"))]
    spec_repository: Optional<String>,
    #[serde(default)]
    #[schemars(!default, regex(pattern = r"^[^/ \t]+/[^/ \t]+$"))]
    issue_repository: Optional<String>,
    issue: CampaignIssue,
    #[schemars(length(min = 1, max = 512))]
    run_id: String,
    #[serde(default)]
    #[schemars(!default, extend("minProperties" = 1))]
    repositories: Optional<BTreeMap<String, RepositoryConfig>>,
    worklist: Worklist,
    #[serde(default)]
    #[schemars(!default, range(min = 1, max = 128))]
    max_tasks: Optional<u64>,
    #[serde(default)]
    #[schemars(!default, range(min = 1, max = 128))]
    max_parallel: Optional<u64>,
    continuation: Continuation,
    #[schemars(regex(pattern = r"^/"))]
    workspace_root: String,
    #[schemars(regex(pattern = r"^/.*/capture/archive$"))]
    capture_root: String,
    #[schemars(regex(pattern = r"^/"))]
    tally: String,
    #[schemars(regex(pattern = r"^/"))]
    driver: String,
    #[schemars(range(min = 1))]
    driver_runtime_max_sec: u64,
    #[serde(default)]
    #[schemars(!default)]
    post_failure_evidence: Optional<bool>,
    #[serde(default)]
    #[schemars(!default)]
    post_failure_stderr: Optional<bool>,
    #[serde(default)]
    #[schemars(!default, length(max = 1000))]
    steering: Optional<Vec<SteeringComment>>,
    #[serde(default)]
    #[schemars(
        !default,
        length(min = 1, max = 128),
        regex(pattern = r"^[^\s/\\\u0000]+$")
    )]
    local_actor: Optional<String>,
    #[serde(default)]
    #[schemars(!default)]
    steering_source: Optional<SteeringSource>,
    #[serde(default)]
    #[schemars(!default, extend("maxProperties" = 128))]
    task_steering: Optional<BTreeMap<String, TaskSteeringComments>>,
    #[serde(default)]
    #[schemars(!default)]
    merge_method: Optional<MergeMethod>,
    steward: Option<CanonicalSteward>,
    #[serde(default)]
    #[schemars(!default)]
    agent: Optional<CanonicalAgent>,
    #[serde(default)]
    #[schemars(
        !default,
        length(min = 1, max = 16),
        extend("uniqueItems" = true)
    )]
    gates: Optional<Vec<CanonicalGate>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
struct TaskSteeringComments(#[schemars(length(max = 1000))] Vec<SteeringComment>);

#[must_use]
pub fn flow_args_schema() -> Value {
    let mut schema = serde_json::to_value(schema_for!(SpecBuildFlowArgs))
        .expect("a derived JSON Schema must serialize");
    let object = schema
        .as_object_mut()
        .expect("the spec-build flow arguments schema must be an object");
    object.remove("$schema");
    object.remove("title");
    object.remove("description");
    schema
}

#[must_use]
pub fn rendered_flow_args_schema_property() -> String {
    let rendered = serde_json::to_string_pretty(&flow_args_schema())
        .expect("a derived JSON Schema must serialize");
    let mut lines = rendered.lines();
    let first = lines.next().expect("a JSON object has a first line");
    let mut output = format!("  argsSchema: {first}");
    for line in lines {
        output.push('\n');
        output.push_str("  ");
        output.push_str(line);
    }
    output.push(',');
    output
}

pub fn replace_generated_flow_args_schema(source: &str) -> Result<String, String> {
    let start = source
        .find(GENERATED_SCHEMA_BEGIN)
        .ok_or_else(|| format!("missing marker {GENERATED_SCHEMA_BEGIN:?}"))?;
    let body_start = source[start..]
        .find('\n')
        .map(|offset| start + offset + 1)
        .ok_or_else(|| "generated schema begin marker has no following line".to_owned())?;
    let body_end = source[body_start..]
        .find(GENERATED_SCHEMA_END)
        .map(|offset| body_start + offset)
        .ok_or_else(|| format!("missing marker {GENERATED_SCHEMA_END:?}"))?;
    if source[body_end..].find(GENERATED_SCHEMA_BEGIN).is_some() {
        return Err("generated schema begin marker appears more than once".to_owned());
    }
    if source[body_end + GENERATED_SCHEMA_END.len()..].contains(GENERATED_SCHEMA_END) {
        return Err("generated schema end marker appears more than once".to_owned());
    }

    let mut output = String::with_capacity(source.len());
    output.push_str(&source[..body_start]);
    output.push_str(&rendered_flow_args_schema_property());
    output.push('\n');
    output.push_str(&source[body_end..]);
    Ok(output)
}

#[must_use]
pub fn generated_schema_block(source: &str) -> Option<&str> {
    let start = source.find(GENERATED_SCHEMA_BEGIN)?;
    let body_start = source[start..]
        .find('\n')
        .map(|offset| start + offset + 1)?;
    let body_end = source[body_start..]
        .find(GENERATED_SCHEMA_END)
        .map(|offset| body_start + offset)?;
    Some(&source[body_start..body_end])
}

#[cfg(test)]
mod tests {
    use super::{
        generated_schema_block, rendered_flow_args_schema_property,
        replace_generated_flow_args_schema, GENERATED_SCHEMA_BEGIN, GENERATED_SCHEMA_END,
    };

    #[test]
    fn schema_replacement_is_bounded_and_idempotent() {
        let source = format!(
            "before\n{GENERATED_SCHEMA_BEGIN}\n  argsSchema: {{}},\n{GENERATED_SCHEMA_END}\nafter\n"
        );
        let replaced = replace_generated_flow_args_schema(&source).unwrap();
        assert!(replaced.starts_with("before\n"));
        assert!(replaced.ends_with("after\n"));
        let expected = format!("{}\n", rendered_flow_args_schema_property());
        assert_eq!(generated_schema_block(&replaced), Some(expected.as_str()));
        assert_eq!(
            replace_generated_flow_args_schema(&replaced).unwrap(),
            replaced
        );
    }
}
