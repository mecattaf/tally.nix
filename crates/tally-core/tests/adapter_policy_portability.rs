//! The portability matrix: a policy-less worklist must render against every
//! adapter in the catalog.
//!
//! The class this guards is a cross-adapter default. Three campaign-contract
//! constants once held one preset's policy names -- `never`,
//! `danger-full-access`, `read-only`, all keys of the codex preset's own maps
//! -- and fired whenever a worklist omitted the keys. Every other adapter then
//! rendered a policy it had never declared and died with "value not authorized
//! by this adapter", quoting bytes the operator never wrote, at exactly the
//! moment an adapter switch is forced: a quota outage. The escape hatch was to
//! null all three keys, which was knowable only by reading the renderer.
//!
//! So the matrix renders admission argv for EVERY catalog adapter against a
//! worklist that names no policy, and asserts every one renders. Any future
//! adapter-flavoured default reintroduced into adapter-neutral bytes fails
//! here, whichever layer it is smuggled through.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tally_core::adapters::{AdapterConfig, AdapterEngine, AdapterJobOptions};
use tally_core::campaign_contract::CampaignAgent;

/// The committed snapshot of `nix/lib/adapters.nix`'s preset catalog.
///
/// It lives inside the crate and resolves through `CARGO_MANIFEST_DIR` because
/// the flake checks build from a filtered source: a fixture at the repository
/// root is simply absent at `/build/source`, and this test would pass in the
/// dev shell while panicking with file-not-found inside the sandbox.
fn catalog() -> BTreeMap<String, AdapterConfig> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/adapter-catalog-presets.json");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

/// The agent a worklist that writes no policy key admits to.
fn policy_less_agent() -> CampaignAgent {
    serde_json::from_value(serde_json::json!({})).expect("an empty agent object admits")
}

#[test]
fn policy_less_worklist_renders_against_every_catalog_adapter_portability() {
    let catalog = catalog();
    assert!(
        catalog.len() >= 4,
        "the catalog snapshot lost adapters: {:?}",
        catalog.keys().collect::<Vec<_>>()
    );
    AdapterEngine::new(&catalog)
        .validate_all()
        .expect("the catalog snapshot is a valid adapter configuration");

    let agent = policy_less_agent();
    assert_eq!(
        agent.approval_policy, None,
        "the contract names no approval policy"
    );
    assert_eq!(
        agent.sandbox_policy, None,
        "the contract names no sandbox policy"
    );
    assert_eq!(
        agent.diagnosis_sandbox_policy, None,
        "the contract names no diagnosis policy"
    );

    let engine = AdapterEngine::new(&catalog);
    for name in catalog.keys() {
        let options = AdapterJobOptions {
            approval_policy: agent.approval_policy.clone(),
            sandbox_policy: agent.sandbox_policy.clone(),
            ..AdapterJobOptions::default()
        };
        engine
            .launch_with_options(name, &agent.argv, &options, None)
            .unwrap_or_else(|error| {
                panic!("adapter {name:?} cannot render a policy-less lane node: {error}")
            });
    }
}

#[test]
fn policy_less_diagnosis_node_renders_against_every_catalog_adapter_portability() {
    let catalog = catalog();
    let engine = AdapterEngine::new(&catalog);
    let agent = policy_less_agent();

    for (name, adapter) in &catalog {
        // A diagnosis node reads rather than writes, so an adapter answers for
        // it separately from its lane; silence still means the adapter's own
        // behaviour rather than a borrowed policy name.
        let sandbox = adapter
            .resolved_diagnosis_sandbox_policy(agent.diagnosis_sandbox_policy.as_deref())
            .map(str::to_owned);
        let options = AdapterJobOptions {
            sandbox_policy: sandbox,
            ..AdapterJobOptions::default()
        };
        engine
            .launch_with_options(name, &agent.argv, &options, None)
            .unwrap_or_else(|error| {
                panic!("adapter {name:?} cannot render a policy-less diagnosis node: {error}")
            });
    }
}

/// The matrix is not vacuous: the exact literals a cross-adapter default would
/// supply still fail against the adapters that never declared them. That is
/// what a policy-less worklist meets on every adapter but one, and what any
/// reintroduced cross-adapter default would meet again.
#[test]
fn the_deleted_cross_adapter_literals_still_refuse_to_render_portability() {
    let catalog = catalog();
    let engine = AdapterEngine::new(&catalog);
    let argv = vec!["payload".to_owned()];

    for name in ["claude-code", "pi", "shell"] {
        let options = AdapterJobOptions {
            approval_policy: Some("never".to_owned()),
            sandbox_policy: Some("danger-full-access".to_owned()),
            ..AdapterJobOptions::default()
        };
        let error = engine
            .launch_with_options(name, &argv, &options, None)
            .expect_err("a codex policy name is not this adapter's vocabulary");
        assert!(
            error
                .to_string()
                .contains("is not authorized by this adapter"),
            "adapter {name:?}: {error}"
        );
    }

    // codex is the one adapter that does declare them, which is exactly why
    // they were mistaken for facts about agents.
    let options = AdapterJobOptions {
        approval_policy: Some("never".to_owned()),
        sandbox_policy: Some("danger-full-access".to_owned()),
        ..AdapterJobOptions::default()
    };
    engine
        .launch_with_options("codex", &argv, &options, None)
        .expect("codex declares both names itself");
}

#[test]
fn catalog_policy_defaults_are_each_adapter_s_own_vocabulary_portability() {
    let catalog = catalog();

    // The codex preset is the one that wanted the deleted constants, and it is
    // the one that now declares them itself.
    let codex = &catalog["codex"];
    assert_eq!(codex.default_approval_policy(), Some("never"));
    assert_eq!(codex.default_sandbox_policy(), Some("danger-full-access"));
    // Not read-only: that name maps to this binary's landlock jailer, which
    // denies every filesystem write including /dev/shm and tempdirs and killed
    // the diagnosing agent's own exec machinery before it could reason.
    assert_eq!(
        codex.default_diagnosis_sandbox_policy(),
        Some("workspace-write")
    );

    for (name, adapter) in &catalog {
        for declared in [
            adapter.default_approval_policy(),
            adapter.default_sandbox_policy(),
            adapter.default_diagnosis_sandbox_policy(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                adapter.launch.approval_policies.contains_key(declared)
                    || adapter.launch.sandbox_policies.contains_key(declared),
                "adapter {name:?} declares default policy {declared:?} it does not itself authorize"
            );
        }
    }

    // An explicit worklist value always wins over the adapter's own answer.
    let codex = &catalog["codex"];
    assert_eq!(
        codex.resolved_sandbox_policy(Some("read-only")),
        Some("read-only")
    );
    assert_eq!(
        codex.resolved_approval_policy(Some("untrusted")),
        Some("untrusted")
    );
}
