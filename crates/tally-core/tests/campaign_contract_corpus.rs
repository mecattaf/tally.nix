use std::env;
use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};
use tally_core::campaign_contract::{
    canonical_json, executable_digest, validate_manifest, CampaignManifest,
    CanonicalCampaignGraphV1, CanonicalCampaignTaskV1,
};

fn accepted_vector(name: &str, raw_manifest: Value, tasks: Vec<CanonicalCampaignTaskV1>) -> Value {
    let manifest: CampaignManifest = serde_json::from_value(raw_manifest)
        .unwrap_or_else(|error| panic!("{name} raw manifest did not deserialize: {error}"));
    validate_manifest(&manifest)
        .unwrap_or_else(|error| panic!("{name} manifest did not validate: {error}"));
    let graph = CanonicalCampaignGraphV1::new(manifest.clone(), tasks)
        .unwrap_or_else(|error| panic!("{name} graph was not canonical: {error}"));
    let manifest_canonical_json = canonical_json(&manifest).unwrap();
    let graph_canonical_json = graph.canonical_json().unwrap();

    json!({
        "name": name,
        "manifest": manifest,
        "manifestCanonicalJson": manifest_canonical_json,
        "graph": graph,
        "graphCanonicalJson": graph_canonical_json,
        "digest": graph.executable_digest,
    })
}

fn accepted_member(accepted: &[Value], name: &str, member: &str) -> Value {
    accepted
        .iter()
        .find(|vector| vector["name"] == name)
        .unwrap_or_else(|| panic!("accepted corpus vector {name:?} is missing"))[member]
        .clone()
}

fn apply_mutation(document: &mut Value, mutation: &Value) {
    let kind = mutation["kind"]
        .as_str()
        .expect("mutation kind must be text");
    let pointer = mutation["pointer"]
        .as_str()
        .expect("mutation pointer must be text");
    let target = if pointer.is_empty() {
        document
    } else {
        document
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("mutation pointer {pointer:?} does not exist"))
    };
    match kind {
        "replace" => {
            *target = mutation["value"].clone();
        }
        "insert" => {
            let key = mutation["key"]
                .as_str()
                .expect("insert mutation key must be text");
            target
                .as_object_mut()
                .expect("insert mutation target must be an object")
                .insert(key.to_owned(), mutation["value"].clone());
        }
        "remove" => {
            let key = mutation["key"]
                .as_str()
                .expect("remove mutation key must be text");
            target
                .as_object_mut()
                .expect("remove mutation target must be an object")
                .remove(key)
                .unwrap_or_else(|| panic!("remove mutation member {key:?} does not exist"));
        }
        other => panic!("unknown corpus mutation kind {other:?}"),
    }
}

fn canonical_manifest_rejects(value: &Value) -> bool {
    let Ok(manifest) = serde_json::from_value::<CampaignManifest>(value.clone()) else {
        return true;
    };
    if validate_manifest(&manifest).is_err() {
        return true;
    }
    canonical_json(&manifest).unwrap() != canonical_json(value).unwrap()
}

fn canonical_graph_rejects(value: &Value) -> bool {
    let Ok(graph) = serde_json::from_value::<CanonicalCampaignGraphV1>(value.clone()) else {
        return true;
    };
    if validate_manifest(&graph.manifest).is_err()
        || canonical_json(&graph).unwrap() != canonical_json(value).unwrap()
    {
        return true;
    }
    match executable_digest(&graph.manifest, &graph.tasks) {
        Ok(digest) => digest != graph.executable_digest,
        Err(_) => true,
    }
}

fn rejection_vector(
    accepted: &[Value],
    name: &str,
    decoder: &str,
    base: &str,
    mutation: Value,
) -> Value {
    let member = match decoder {
        "manifest" => "manifest",
        "graph" => "graph",
        other => panic!("unknown corpus decoder {other:?}"),
    };
    let mut rejected = accepted_member(accepted, base, member);
    apply_mutation(&mut rejected, &mutation);
    let rust_rejected = match decoder {
        "manifest" => canonical_manifest_rejects(&rejected),
        "graph" => canonical_graph_rejects(&rejected),
        _ => unreachable!(),
    };
    assert!(rust_rejected, "Rust accepted rejection vector {name:?}");

    json!({
        "name": name,
        "decoder": decoder,
        "base": base,
        "mutation": mutation,
    })
}

fn kebab_case(value: &str) -> String {
    let mut rendered = String::new();
    for character in value.chars() {
        if character.is_ascii_uppercase() {
            rendered.push('-');
            rendered.push(character.to_ascii_lowercase());
        } else {
            rendered.push(character);
        }
    }
    rendered
}

fn generated_corpus() -> String {
    let minimal = accepted_vector(
        "minimal-defaulted-serial",
        json!({
            "schemaVersion": 1,
            "name": "corpus-minimal",
            "repository": {
                "checkout": "/srv/contract-corpus/minimal",
                "forge": "local"
            },
            "agent": {},
            "gates": [{
                "kind": "forbidPaths",
                "id": "scope",
                "forbidPaths": ["*.db"]
            }],
            "tasks": [{
                "id": "build",
                "kind": "implementation",
                "issue": 101
            }]
        }),
        vec![CanonicalCampaignTaskV1 {
            number: 101,
            title: "Build the fixture".to_owned(),
            body: "Implement the minimal admitted fixture.".to_owned(),
        }],
    );

    let complete = accepted_vector(
        "complete-parallel-mixed",
        json!({
            "schemaVersion": 1,
            "name": "corpus-complete",
            "repository": {
                "checkout": "/srv/contract-corpus/complete",
                "baseBranch": "trunk",
                "remote": "upstream",
                "forge": "github"
            },
            "maxTasks": 4,
            "maxParallel": 2,
            "driverRuntimeMaxSec": 900,
            "runtimeMaxSec": null,
            "pool": "campaign-corpus",
            "mergeMethod": "merge",
            "agent": {
                "adapter": "codex",
                "argv": ["read the admitted brief"],
                "priority": "medium",
                "runtimeMaxSec": null,
                "approvalPolicy": "never",
                "sandboxPolicy": "danger-full-access",
                "diagnosisSandboxPolicy": "read-only",
                "model": "m".repeat(128)
            },
            "steward": {
                "adapter": "narrator",
                "argv": ["narrate", "--json"],
                "env": {"NARRATOR_ENDPOINT": "https://narrator.invalid/v1"},
                "finalMessagePattern": r"^(?:résultat: )([A-Z\d_-]+)$",
                "runtimeMaxSec": 240
            },
            "gates": [
                {
                    "kind": "command",
                    "id": "tests",
                    "preflightArgv": ["true"],
                    "argv": ["cargo", "test"],
                    "runtimeMaxSec": 600
                },
                {
                    "kind": "forbidPaths",
                    "id": "scope",
                    "forbidPaths": ["*.db", "target/**"],
                    "runtimeMaxSec": 30
                }
            ],
            "tasks": [
                {
                    "id": "build",
                    "kind": "implementation",
                    "issue": 201,
                    "dependencies": [],
                    "conflictDomains": ["src/build", "SRC/build"]
                },
                {
                    "id": "verify",
                    "kind": "checkpoint",
                    "issue": 202,
                    "dependencies": ["build"],
                    "argv": ["cargo", "test"],
                    "runtimeMaxSec": 300
                }
            ]
        }),
        vec![
            CanonicalCampaignTaskV1 {
                number: 201,
                title: "Implement the thing".to_owned(),
                body: "Brief for the implementation task.".to_owned(),
            },
            CanonicalCampaignTaskV1 {
                number: 202,
                title: "Verify the thing".to_owned(),
                body: "Run the admitted verification.".to_owned(),
            },
        ],
    );

    let accepted = vec![minimal, complete];
    let complete_manifest = accepted_member(&accepted, "complete-parallel-mixed", "manifest");
    let mut manifest_fields = complete_manifest
        .as_object()
        .expect("accepted manifest must be an object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    manifest_fields.sort();
    let complete_graph = accepted_member(&accepted, "complete-parallel-mixed", "graph");
    let mut graph_fields = complete_graph
        .as_object()
        .expect("accepted graph must be an object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    graph_fields.sort();

    let mut rejected = Vec::new();
    for field in &manifest_fields {
        rejected.push(rejection_vector(
            &accepted,
            &format!("missing-manifest-{}", kebab_case(field)),
            "manifest",
            "complete-parallel-mixed",
            json!({"kind": "remove", "pointer": "", "key": field}),
        ));
    }
    for (name, pointer) in [
        ("unknown-manifest", ""),
        ("unknown-repository", "/repository"),
        ("unknown-agent", "/agent"),
        ("unknown-steward", "/steward"),
        ("unknown-command-gate", "/gates/0"),
        ("unknown-forbid-gate", "/gates/1"),
        ("unknown-implementation-task", "/tasks/0"),
        ("unknown-checkpoint-task", "/tasks/1"),
    ] {
        rejected.push(rejection_vector(
            &accepted,
            name,
            "manifest",
            "complete-parallel-mixed",
            json!({"kind": "insert", "pointer": pointer, "key": "typo", "value": true}),
        ));
    }
    for (name, pointer, value) in [
        (
            "conflict-domains-empty",
            "/tasks/0/conflictDomains",
            json!([]),
        ),
        (
            "conflict-domains-duplicate",
            "/tasks/0/conflictDomains",
            json!(["src/build", "src/build"]),
        ),
        (
            "conflict-domains-parent",
            "/tasks/0/conflictDomains/0",
            json!("src/../build"),
        ),
        (
            "forbid-trailing-slash",
            "/gates/1/forbidPaths/0",
            json!("tmp/"),
        ),
        (
            "forbid-duplicate",
            "/gates/1/forbidPaths",
            json!(["*.db", "*.db"]),
        ),
        ("model-129", "/agent/model", json!("m".repeat(129))),
        (
            "regex-invalid-syntax",
            "/steward/finalMessagePattern",
            json!("("),
        ),
        (
            "regex-zero-captures",
            "/steward/finalMessagePattern",
            json!("^result: .*$"),
        ),
        (
            "regex-two-captures",
            "/steward/finalMessagePattern",
            json!("^(a)(b)$"),
        ),
        (
            "regex-backreference",
            "/steward/finalMessagePattern",
            json!(r"^(a)\1$"),
        ),
        (
            "regex-lookaround",
            "/steward/finalMessagePattern",
            json!("^(?=(.*))$"),
        ),
        (
            "regex-named-group",
            "/steward/finalMessagePattern",
            json!("^(?P<answer>.*)$"),
        ),
        (
            "regex-inline-flags",
            "/steward/finalMessagePattern",
            json!("(?i)^(.*)$"),
        ),
        (
            "pattern-explicit-null",
            "/steward/finalMessagePattern",
            Value::Null,
        ),
        (
            "steward-env-value-bound",
            "/steward/env/NARRATOR_ENDPOINT",
            json!("v".repeat(4097)),
        ),
        (
            "steward-argv-control",
            "/steward/argv",
            json!(["narrate\nnow"]),
        ),
    ] {
        rejected.push(rejection_vector(
            &accepted,
            name,
            "manifest",
            "complete-parallel-mixed",
            json!({"kind": "replace", "pointer": pointer, "value": value}),
        ));
    }
    let oversized_env = (0..=64)
        .map(|index| (format!("KEY_{index}"), json!("value")))
        .collect::<Map<_, _>>();
    rejected.push(rejection_vector(
        &accepted,
        "steward-env-entry-bound",
        "manifest",
        "complete-parallel-mixed",
        json!({"kind": "replace", "pointer": "/steward/env", "value": oversized_env}),
    ));

    for field in &graph_fields {
        rejected.push(rejection_vector(
            &accepted,
            &format!("missing-graph-{}", kebab_case(field)),
            "graph",
            "complete-parallel-mixed",
            json!({"kind": "remove", "pointer": "", "key": field}),
        ));
    }
    rejected.push(rejection_vector(
        &accepted,
        "unknown-graph",
        "graph",
        "complete-parallel-mixed",
        json!({"kind": "insert", "pointer": "", "key": "typo", "value": true}),
    ));
    rejected.push(rejection_vector(
        &accepted,
        "unknown-canonical-task",
        "graph",
        "complete-parallel-mixed",
        json!({"kind": "insert", "pointer": "/tasks/0", "key": "typo", "value": true}),
    ));
    rejected.push(rejection_vector(
        &accepted,
        "graph-digest-mismatch",
        "graph",
        "complete-parallel-mixed",
        json!({
            "kind": "replace",
            "pointer": "/executableDigest",
            "value": format!("sha256:{}", "0".repeat(64))
        }),
    ));

    let corpus = json!({
        "schemaVersion": 1,
        "comment": "Generated by crates/tally-core/tests/campaign_contract_corpus.rs from the Rust campaign contract. Python consumes these canonical bytes and mutations. pullRequestWorkspaceFixture is the required-key sentinel for the two #471 regression fixtures.",
        "requiredKeySets": {
            "campaignManifest": manifest_fields,
            "campaignGraph": graph_fields,
            "pullRequestWorkspaceFixture": ["baseRev", "publishBranch", "taskId"]
        },
        "accepted": accepted,
        "rejected": rejected,
    });
    format!("{}\n", serde_json::to_string_pretty(&corpus).unwrap())
}

#[test]
fn checked_in_campaign_contract_corpus_matches_rust() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test/fixtures/spec-build/contract-corpus.json");
    let generated = generated_corpus();
    if env::var_os("TALLY_UPDATE_CONTRACT_CORPUS").is_some() {
        fs::write(&path, &generated)
            .unwrap_or_else(|error| panic!("cannot update {}: {error}", path.display()));
    }
    let checked_in = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    assert_eq!(
        checked_in,
        generated,
        "{} is stale; regenerate it from this test with TALLY_UPDATE_CONTRACT_CORPUS=1",
        path.display()
    );
}
