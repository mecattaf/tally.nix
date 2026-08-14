use serde_json::{json, Value};
use tally_core::campaign_folds::{
    campaign_digest, completion_trailer_block, render_campaign_summary, stable_publish_branch,
    CampaignDigest, CampaignFoldError, CampaignReconciliation,
};

const SHA_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

const STAGE_ZERO_DIAGNOSIS_ONE: &str = "Identified `chapter-gate` as a publication-order failure: `test/fleet-gate.sh` queried GitHub for the local HEAD before running any ladder stage, but that commit was not available remotely.\n\n- Push the intended publish branch and ensure an open PR has that exact HEAD SHA, or run against the current remote `main` tip.\n- Confirm the commit resolves through `gh api` before retrying.\n- No source fix is indicated; the final conformance bar never ran because the fleet gate exited first.\n\nCheckpoint capture: /home/tom/.local/state/tally/capture/archive/019ffbfe-af3d-74e3-95ee-53fdd9b5348d.chapter-gate/checkpoint.json";
const STAGE_ZERO_DIAGNOSIS_TWO: &str = "Observed chapter-gate fail before the gate ladder because test/fleet-gate.sh queried GitHub for an unpublished HEAD commit.\n\n- Push the intended publish branch and open a PR whose head exactly matches local HEAD.\n- Verify GitHub resolves `$(git rev-parse HEAD)` before retrying.\n- Do not change source; the final conformance bar never started.\n\nCheckpoint capture: /home/tom/.local/state/tally/capture/archive/019ffbff-5e9e-7a60-ad2d-52a319b40ec3.chapter-gate/checkpoint.json";

// Observed from durable ref
// refs/tally/spec-build/v1/34af00568cc43499aa8bcc35/summary/archive/eps0-quiescent
// (blob 604dc3e4143695bc02cfb36090ef176545d9418e).
const STAGE_ZERO_QUIESCENT_SUMMARY: &str = r#"### Campaign closed at frontier quiescence

Settled 3 of 4 task(s) against durable merge/checkpoint facts.
Worklist `sha256:51acb21afb57b525a222fd78a0af175b15438c730f671e00c50916f3623e20f6` at `6a7c841a6fe2604aa934503c776373d96be9496c`.

Blocked: 1 · Outstanding: 0 · Steering notes issued: 2 · Machinery retries: 0

#### Merged

- `final-bar-local-forge-repair` — Repair the final conformance bar for the post-chapter-2 local-only CLI (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/final-bar-local-forge-repair-fe698d4aca9681f6)
- `gate-keep-going` — Make the fleet gate enumerate every failing flake attribute (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/gate-keep-going-18d1894dad6d1cf7)
- `steering-grammar-negation` — Let machine diagnoses state a negation without being gagged (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/steering-grammar-negation-b3221473ff0fbb83)

#### Blocked

- `chapter-gate` — Epsilon stage 0 gate: full gate ladder plus the final conformance bar; blocked by `chapter-gate`; 2 steered attempt(s)

#### Steering notes issued

- `chapter-gate` attempt 1: Identified `chapter-gate` as a publication-order failure: `test/fleet-gate.sh` queried GitHub for the local HEAD before running any ladder stage, but that co...
- `chapter-gate` attempt 2: Observed chapter-gate fail before the gate ladder because test/fleet-gate.sh queried GitHub for an unpublished HEAD commit. - Push the intended publish branc...
"#;

// Observed from durable ref
// refs/tally/spec-build/v1/34af00568cc43499aa8bcc35/summary/archive/eps1-complete
// (blob 1a269d38cafc7703491dfeed8baf96059589a636).
const STAGE_ONE_COMPLETE_SUMMARY: &str = r#"### Campaign complete

Settled 15 of 15 task(s) against durable merge/checkpoint facts.
Worklist `sha256:60669d21e08d0b6ca8c0e21f06d9f789d6062425f8e32d27a17189d986b7706e` at `c848d4919d5c4d31437baf495355b18477534425`.

#### Merged

- `campaign-nix-surface-retire` — Retire the vestigial module campaign rendering (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/campaign-nix-surface-retire-10e29829dfe0d19a)
- `delete-gh-inbound-core` — Delete the gh-inbound producers stack and the daemon's outbound gh subsystem (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-inbound-core-3f04a463e248ca7a)
- `delete-gh-origin-durable` — Delete the GhOrigin durable surface, keeping the ledger decode arm (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-origin-durable-987ae046cd9df2e2)
- `delete-gh-nix-tree` — Collapse the nix tree to local-only campaign operation (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-nix-tree-a1eb61c66a032765)
- `squash-migration-modules` — Delete the capture and unit-exit migration modules and the migrate verb (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-migration-modules-bfe309c1350da2cd)
- `squash-err-fallbacks` — Delete the legacy stderr-capture dual-read arms (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-err-fallbacks-cbae9d31367e3347)
- `squash-rowversion-ladder` — Replace the row-migration ladder with a floor refusal (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-rowversion-ladder-2b7396a60cf4118f)
- `brief-carries-conflict-domains` — Render conflictDomains into the projected task brief (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/brief-carries-conflict-domains-f40bf45d2d4c4271)
- `dead-cuts` — Sweep the dead operational surfaces (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/dead-cuts-7acfea6acf4c5511)
- `ownership-preflight-warn` — Warn at arm time when task text names paths outside its domains (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/ownership-preflight-warn-0f89346412c96d91)
- `poll-liveness-arm` — Poll dispatches when work is dispatchable and nothing runs (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/poll-liveness-arm-b64f573d2c93382f)
- `status-renders-reconciled-truth` — Status renders the last reconciled truth, never a placeholder (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/status-renders-reconciled-truth-d19c2a7417a18c32)
- `producers-config-variant-box` — Box the Calendar producer variant for the post-deletion enum shape (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/producers-config-variant-box-99e6acbc8d4e2c19)
- `final-bar-stage1-reseat` — Reseat the final conformance bar on the stage 1 contracts (local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/final-bar-stage1-reseat-822864d640c0c6d5)

#### Checkpoints passed

- `chapter-gate` — Epsilon stage 1 gate: full gate ladder plus the final conformance bar at `6afee3aa4b0eb1a8501876c90ba88197cd2a36dd`

#### Reconciler warnings

- campaign pardon local://campaign/epsilon/attempt-receipts/4 pardoned 3 earlier machine receipt(s) for task(s) 'chapter-gate'
- campaign pardon local://campaign/epsilon/attempt-receipts/7 pardoned 2 earlier machine receipt(s)
- campaign pardon local://campaign/epsilon/attempt-receipts/10 pardoned 2 earlier machine receipt(s)
- campaign pardon local://campaign/epsilon/attempt-receipts/15 pardoned 4 earlier machine receipt(s)
- campaign pardon local://campaign/epsilon/attempt-receipts/18 pardoned 2 earlier machine receipt(s)
"#;

struct DigestParityCase {
    name: &'static str,
    reconciliation: Value,
    outcome: &'static str,
    expected_digest: Value,
    expected_summary: &'static str,
}

#[test]
fn epsilon_digests_and_summaries_match_the_python_durable_outputs() {
    for case in [stage_zero_quiescent_case(), stage_one_complete_case()] {
        let reconciliation: CampaignReconciliation =
            serde_json::from_value(case.reconciliation).unwrap();
        let digest = campaign_digest(&reconciliation, case.outcome);

        assert_eq!(
            serde_json::to_value(&digest).unwrap(),
            case.expected_digest,
            "{} digest",
            case.name
        );
        assert_eq!(
            render_campaign_summary(&digest),
            case.expected_summary,
            "{} summary",
            case.name
        );
    }
}

#[test]
fn epsilon_publish_branches_match_the_durable_stage_refs() {
    struct Case {
        name: &'static str,
        campaign: &'static str,
        campaign_id: &'static str,
        task_id: &'static str,
        revision: Option<&'static str>,
        expected: &'static str,
    }

    let cases = [
        Case {
            name: "epsilon stage 0",
            campaign: "epsilon",
            campaign_id: "019ffbb8-8193-7c40-b48e-b34e13246b21",
            task_id: "final-bar-local-forge-repair",
            revision: Some(
                "sha256:fe698d4aca9681f639a5e96ed5419913c71e8717ed510b5d7719d5ba147d0147",
            ),
            expected: "tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/final-bar-local-forge-repair-fe698d4aca9681f6",
        },
        Case {
            name: "epsilon stage 1",
            campaign: "epsilon",
            campaign_id: "019ffc34-90ba-7a52-a907-a539c01f74d7",
            task_id: "status-renders-reconciled-truth",
            revision: Some(
                "sha256:d19c2a7417a18c327d7120013e42989a3da2b15eacf45aa7f4d814f218d79cbd",
            ),
            expected: "tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/status-renders-reconciled-truth-d19c2a7417a18c32",
        },
        Case {
            name: "Python slug normalization and absent revision",
            campaign: "---",
            campaign_id: "arm / # 7",
            task_id: "task-1",
            revision: None,
            expected: "tally/campaign-campaign-arm-7/task-1",
        },
    ];

    for case in cases {
        assert_eq!(
            stable_publish_branch(case.campaign, case.campaign_id, case.task_id, case.revision,),
            case.expected,
            "{}",
            case.name
        );
    }
}

#[test]
fn completion_trailers_match_the_python_grammar() {
    let valid = [
        (
            "task-1",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            "final-bar-stage1-reseat",
            "sha256:822864d640c0c6d5b541e61e3e82be9f829d2f770253db3f9fc804b28dcf88f9",
        ),
    ];
    for (task_id, revision) in valid {
        assert_eq!(
            completion_trailer_block(task_id, revision).unwrap(),
            format!("Tally-Task: {task_id}\nTally-Revision: {revision}")
        );
    }

    let invalid = [
        (
            "not a task",
            SHA_A,
            CampaignFoldError::UnsafeCompletionTaskId,
        ),
        ("task-", SHA_A, CampaignFoldError::UnsafeCompletionTaskId),
        ("Task", SHA_A, CampaignFoldError::UnsafeCompletionTaskId),
        (
            "task-1",
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            CampaignFoldError::InvalidCompletionRevision,
        ),
        (
            "task-1",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            CampaignFoldError::InvalidCompletionRevision,
        ),
    ];
    for (task_id, revision, expected) in invalid {
        assert_eq!(
            completion_trailer_block(task_id, revision).unwrap_err(),
            expected
        );
    }
}

#[test]
fn summary_sections_keep_the_python_row_bounds_and_unicode_character_count() {
    let outstanding = (0..42)
        .map(|index| format!("task-{index:02}"))
        .collect::<Vec<_>>();
    let warnings = (0..14)
        .map(|index| format!("warning {index:02}"))
        .collect::<Vec<_>>();
    let digest: CampaignDigest = serde_json::from_value(json!({
        "schemaVersion": 1,
        "campaign": "fixture",
        "repository": "acme/code",
        "outcome": "quiescent",
        "source": {
            "sha256": SHA_A,
            "revision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        },
        "baseRevision": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "taskCount": 43,
        "merged": [{
            "taskId": "merged",
            "title": format!("{}   tail", "é".repeat(80)),
            "pullRequest": "local://acme/code/merged",
            "mergeCommit": "cccccccccccccccccccccccccccccccccccccccc"
        }],
        "checkpoints": [],
        "blocked": [],
        "outstanding": outstanding,
        "steering": [],
        "retries": [{"taskId": "task-00", "attempt": 1, "summary": "Retried it."}],
        "deferrals": ["gate"],
        "warnings": warnings
    }))
    .unwrap();

    let summary = render_campaign_summary(&digest);
    assert!(summary.contains(&format!("{}...", "é".repeat(77))));
    assert!(summary.contains("- `task-39`"));
    assert!(!summary.contains("- `task-40`"));
    assert!(summary.contains("- warning 11"));
    assert!(!summary.contains("- warning 12"));
    assert_eq!(summary.matches("- …and 2 more").count(), 2);
    assert!(summary.contains("#### Checkpoints deferred by outstanding work"));
    assert!(summary.contains("#### Campaign machinery faults"));
    assert!(summary.ends_with('\n'));
}

fn stage_zero_quiescent_case() -> DigestParityCase {
    let source = json!({
        "path": "silent-factory-worklists/epsilon.json",
        "sha256": "sha256:51acb21afb57b525a222fd78a0af175b15438c730f671e00c50916f3623e20f6",
        "revision": "6a7c841a6fe2604aa934503c776373d96be9496c"
    });
    let tasks = json!([
        {
            "id": "final-bar-local-forge-repair",
            "title": "Repair the final conformance bar for the post-chapter-2 local-only CLI"
        },
        {
            "id": "gate-keep-going",
            "title": "Make the fleet gate enumerate every failing flake attribute"
        },
        {
            "id": "steering-grammar-negation",
            "title": "Let machine diagnoses state a negation without being gagged"
        },
        {
            "id": "chapter-gate",
            "title": "Epsilon stage 0 gate: full gate ladder plus the final conformance bar"
        }
    ]);
    let merged = json!([
        {
            "taskId": "final-bar-local-forge-repair",
            "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/final-bar-local-forge-repair-fe698d4aca9681f6",
            "mergeCommit": "e317edc290bd2eb2ed7b95f5449d2cc953353960"
        },
        {
            "taskId": "gate-keep-going",
            "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/gate-keep-going-18d1894dad6d1cf7",
            "mergeCommit": "d0134f62773979045cc8edbebdd02f467d929aa1"
        },
        {
            "taskId": "steering-grammar-negation",
            "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/steering-grammar-negation-b3221473ff0fbb83",
            "mergeCommit": "3bb6605933a681968ecb01ad82beac490a5e9555"
        }
    ]);
    let diagnoses = json!([
        {"taskId": "chapter-gate", "attempt": 1, "diagnosis": STAGE_ZERO_DIAGNOSIS_ONE},
        {"taskId": "chapter-gate", "attempt": 2, "diagnosis": STAGE_ZERO_DIAGNOSIS_TWO}
    ]);

    DigestParityCase {
        name: "epsilon stage 0 quiescent",
        reconciliation: json!({
            "schemaVersion": 1,
            "campaign": "epsilon",
            "repository": "mecattaf/tally.nix",
            "source": source,
            "baseRevision": "6a7c841a6fe2604aa934503c776373d96be9496c",
            "tasks": tasks,
            "merged": merged,
            "checkpoints": [],
            "remaining": ["chapter-gate"],
            "frontier": [],
            "diagnoses": diagnoses,
            "retries": [],
            "deferrals": [],
            "blocked": [{"taskId": "chapter-gate", "blockedBy": ["chapter-gate"]}],
            "quiescent": true,
            "warnings": []
        }),
        outcome: "quiescent",
        expected_digest: json!({
            "schemaVersion": 1,
            "campaign": "epsilon",
            "repository": "mecattaf/tally.nix",
            "outcome": "quiescent",
            "source": {
                "path": "silent-factory-worklists/epsilon.json",
                "sha256": "sha256:51acb21afb57b525a222fd78a0af175b15438c730f671e00c50916f3623e20f6",
                "revision": "6a7c841a6fe2604aa934503c776373d96be9496c"
            },
            "baseRevision": "6a7c841a6fe2604aa934503c776373d96be9496c",
            "taskCount": 4,
            "merged": [
                {
                    "taskId": "final-bar-local-forge-repair",
                    "title": "Repair the final conformance bar for the post-chapter-2 local-only CLI",
                    "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/final-bar-local-forge-repair-fe698d4aca9681f6",
                    "mergeCommit": "e317edc290bd2eb2ed7b95f5449d2cc953353960"
                },
                {
                    "taskId": "gate-keep-going",
                    "title": "Make the fleet gate enumerate every failing flake attribute",
                    "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/gate-keep-going-18d1894dad6d1cf7",
                    "mergeCommit": "d0134f62773979045cc8edbebdd02f467d929aa1"
                },
                {
                    "taskId": "steering-grammar-negation",
                    "title": "Let machine diagnoses state a negation without being gagged",
                    "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffbb8-8193-7c40-b48e-b34e13246b21/steering-grammar-negation-b3221473ff0fbb83",
                    "mergeCommit": "3bb6605933a681968ecb01ad82beac490a5e9555"
                }
            ],
            "checkpoints": [],
            "blocked": [{
                "taskId": "chapter-gate",
                "title": "Epsilon stage 0 gate: full gate ladder plus the final conformance bar",
                "blockedBy": ["chapter-gate"],
                "attempts": 2
            }],
            "outstanding": [],
            "steering": [
                {
                    "taskId": "chapter-gate",
                    "attempt": 1,
                    "summary": "Identified `chapter-gate` as a publication-order failure: `test/fleet-gate.sh` queried GitHub for the local HEAD before running any ladder stage, but that co..."
                },
                {
                    "taskId": "chapter-gate",
                    "attempt": 2,
                    "summary": "Observed chapter-gate fail before the gate ladder because test/fleet-gate.sh queried GitHub for an unpublished HEAD commit. - Push the intended publish branc..."
                }
            ],
            "retries": [],
            "deferrals": [],
            "warnings": []
        }),
        expected_summary: STAGE_ZERO_QUIESCENT_SUMMARY,
    }
}

fn stage_one_complete_case() -> DigestParityCase {
    let source = json!({
        "path": "silent-factory-worklists/epsilon.json",
        "sha256": "sha256:60669d21e08d0b6ca8c0e21f06d9f789d6062425f8e32d27a17189d986b7706e",
        "revision": "c848d4919d5c4d31437baf495355b18477534425"
    });
    let tasks = json!([
        {"id": "campaign-nix-surface-retire", "title": "Retire the vestigial module campaign rendering"},
        {"id": "delete-gh-inbound-core", "title": "Delete the gh-inbound producers stack and the daemon's outbound gh subsystem"},
        {"id": "delete-gh-origin-durable", "title": "Delete the GhOrigin durable surface, keeping the ledger decode arm"},
        {"id": "delete-gh-nix-tree", "title": "Collapse the nix tree to local-only campaign operation"},
        {"id": "squash-migration-modules", "title": "Delete the capture and unit-exit migration modules and the migrate verb"},
        {"id": "squash-err-fallbacks", "title": "Delete the legacy stderr-capture dual-read arms"},
        {"id": "squash-rowversion-ladder", "title": "Replace the row-migration ladder with a floor refusal"},
        {"id": "brief-carries-conflict-domains", "title": "Render conflictDomains into the projected task brief"},
        {"id": "dead-cuts", "title": "Sweep the dead operational surfaces"},
        {"id": "ownership-preflight-warn", "title": "Warn at arm time when task text names paths outside its domains"},
        {"id": "poll-liveness-arm", "title": "Poll dispatches when work is dispatchable and nothing runs"},
        {"id": "status-renders-reconciled-truth", "title": "Status renders the last reconciled truth, never a placeholder"},
        {"id": "producers-config-variant-box", "title": "Box the Calendar producer variant for the post-deletion enum shape"},
        {"id": "final-bar-stage1-reseat", "title": "Reseat the final conformance bar on the stage 1 contracts"},
        {"id": "chapter-gate", "title": "Epsilon stage 1 gate: full gate ladder plus the final conformance bar"}
    ]);
    let merged = json!([
        {"taskId": "campaign-nix-surface-retire", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/campaign-nix-surface-retire-10e29829dfe0d19a", "mergeCommit": "4e42c51f75a96a0500a6207a116c209ec7b0f16a"},
        {"taskId": "delete-gh-inbound-core", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-inbound-core-3f04a463e248ca7a", "mergeCommit": "a3bdf6a16dfb13f881f5d5d508cfafb90c5c0a79"},
        {"taskId": "delete-gh-origin-durable", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-origin-durable-987ae046cd9df2e2", "mergeCommit": "47f425778fdf6e45dbbdaa8488289446f4dda809"},
        {"taskId": "delete-gh-nix-tree", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-nix-tree-a1eb61c66a032765", "mergeCommit": "47a809518069687c39c60309b7e8fe0136a418f7"},
        {"taskId": "squash-migration-modules", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-migration-modules-bfe309c1350da2cd", "mergeCommit": "77f43e53490e7df05ce081767fdfd0708dd79e02"},
        {"taskId": "squash-err-fallbacks", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-err-fallbacks-cbae9d31367e3347", "mergeCommit": "4471994bd6906ab72c8dd2250bd6779d9423de03"},
        {"taskId": "squash-rowversion-ladder", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-rowversion-ladder-2b7396a60cf4118f", "mergeCommit": "d30e23f723720f52e0683d783112be9d24c8170c"},
        {"taskId": "brief-carries-conflict-domains", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/brief-carries-conflict-domains-f40bf45d2d4c4271", "mergeCommit": "34acba6975cd25235efb3717f2a20feb999b7f67"},
        {"taskId": "dead-cuts", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/dead-cuts-7acfea6acf4c5511", "mergeCommit": "e4e43ee1b3fad4086df29e5b17bb574984000ec5"},
        {"taskId": "ownership-preflight-warn", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/ownership-preflight-warn-0f89346412c96d91", "mergeCommit": "b3d7ba2fe90f965ae8f6e0549dd53f3f3495548f"},
        {"taskId": "poll-liveness-arm", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/poll-liveness-arm-b64f573d2c93382f", "mergeCommit": "cd27d4b10b0a8858c2cca4d69f194eca8aae2bda"},
        {"taskId": "status-renders-reconciled-truth", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/status-renders-reconciled-truth-d19c2a7417a18c32", "mergeCommit": "816ed305aed9ab96309483f2fe9ac39155c56c8e"},
        {"taskId": "producers-config-variant-box", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/producers-config-variant-box-99e6acbc8d4e2c19", "mergeCommit": "782e4c32e8a22b80dd0f6c760cd3e432ebde888c"},
        {"taskId": "final-bar-stage1-reseat", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/final-bar-stage1-reseat-822864d640c0c6d5", "mergeCommit": "6afee3aa4b0eb1a8501876c90ba88197cd2a36dd"}
    ]);
    let warnings = json!([
        "campaign pardon local://campaign/epsilon/attempt-receipts/4 pardoned 3 earlier machine receipt(s) for task(s) 'chapter-gate'",
        "campaign pardon local://campaign/epsilon/attempt-receipts/7 pardoned 2 earlier machine receipt(s)",
        "campaign pardon local://campaign/epsilon/attempt-receipts/10 pardoned 2 earlier machine receipt(s)",
        "campaign pardon local://campaign/epsilon/attempt-receipts/15 pardoned 4 earlier machine receipt(s)",
        "campaign pardon local://campaign/epsilon/attempt-receipts/18 pardoned 2 earlier machine receipt(s)"
    ]);

    DigestParityCase {
        name: "epsilon stage 1 complete",
        reconciliation: json!({
            "schemaVersion": 1,
            "campaign": "epsilon",
            "repository": "mecattaf/tally.nix",
            "source": source,
            "baseRevision": "c848d4919d5c4d31437baf495355b18477534425",
            "tasks": tasks,
            "merged": merged,
            "checkpoints": [{"taskId": "chapter-gate", "revision": "6afee3aa4b0eb1a8501876c90ba88197cd2a36dd"}],
            "remaining": [],
            "frontier": [],
            "diagnoses": [],
            "retries": [],
            "deferrals": [],
            "blocked": [],
            "complete": true,
            "warnings": warnings
        }),
        outcome: "complete",
        expected_digest: json!({
            "schemaVersion": 1,
            "campaign": "epsilon",
            "repository": "mecattaf/tally.nix",
            "outcome": "complete",
            "source": {
                "path": "silent-factory-worklists/epsilon.json",
                "sha256": "sha256:60669d21e08d0b6ca8c0e21f06d9f789d6062425f8e32d27a17189d986b7706e",
                "revision": "c848d4919d5c4d31437baf495355b18477534425"
            },
            "baseRevision": "c848d4919d5c4d31437baf495355b18477534425",
            "taskCount": 15,
            "merged": [
                {"taskId": "campaign-nix-surface-retire", "title": "Retire the vestigial module campaign rendering", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/campaign-nix-surface-retire-10e29829dfe0d19a", "mergeCommit": "4e42c51f75a96a0500a6207a116c209ec7b0f16a"},
                {"taskId": "delete-gh-inbound-core", "title": "Delete the gh-inbound producers stack and the daemon's outbound gh subsystem", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-inbound-core-3f04a463e248ca7a", "mergeCommit": "a3bdf6a16dfb13f881f5d5d508cfafb90c5c0a79"},
                {"taskId": "delete-gh-origin-durable", "title": "Delete the GhOrigin durable surface, keeping the ledger decode arm", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-origin-durable-987ae046cd9df2e2", "mergeCommit": "47f425778fdf6e45dbbdaa8488289446f4dda809"},
                {"taskId": "delete-gh-nix-tree", "title": "Collapse the nix tree to local-only campaign operation", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/delete-gh-nix-tree-a1eb61c66a032765", "mergeCommit": "47a809518069687c39c60309b7e8fe0136a418f7"},
                {"taskId": "squash-migration-modules", "title": "Delete the capture and unit-exit migration modules and the migrate verb", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-migration-modules-bfe309c1350da2cd", "mergeCommit": "77f43e53490e7df05ce081767fdfd0708dd79e02"},
                {"taskId": "squash-err-fallbacks", "title": "Delete the legacy stderr-capture dual-read arms", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-err-fallbacks-cbae9d31367e3347", "mergeCommit": "4471994bd6906ab72c8dd2250bd6779d9423de03"},
                {"taskId": "squash-rowversion-ladder", "title": "Replace the row-migration ladder with a floor refusal", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/squash-rowversion-ladder-2b7396a60cf4118f", "mergeCommit": "d30e23f723720f52e0683d783112be9d24c8170c"},
                {"taskId": "brief-carries-conflict-domains", "title": "Render conflictDomains into the projected task brief", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/brief-carries-conflict-domains-f40bf45d2d4c4271", "mergeCommit": "34acba6975cd25235efb3717f2a20feb999b7f67"},
                {"taskId": "dead-cuts", "title": "Sweep the dead operational surfaces", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/dead-cuts-7acfea6acf4c5511", "mergeCommit": "e4e43ee1b3fad4086df29e5b17bb574984000ec5"},
                {"taskId": "ownership-preflight-warn", "title": "Warn at arm time when task text names paths outside its domains", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/ownership-preflight-warn-0f89346412c96d91", "mergeCommit": "b3d7ba2fe90f965ae8f6e0549dd53f3f3495548f"},
                {"taskId": "poll-liveness-arm", "title": "Poll dispatches when work is dispatchable and nothing runs", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/poll-liveness-arm-b64f573d2c93382f", "mergeCommit": "cd27d4b10b0a8858c2cca4d69f194eca8aae2bda"},
                {"taskId": "status-renders-reconciled-truth", "title": "Status renders the last reconciled truth, never a placeholder", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/status-renders-reconciled-truth-d19c2a7417a18c32", "mergeCommit": "816ed305aed9ab96309483f2fe9ac39155c56c8e"},
                {"taskId": "producers-config-variant-box", "title": "Box the Calendar producer variant for the post-deletion enum shape", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/producers-config-variant-box-99e6acbc8d4e2c19", "mergeCommit": "782e4c32e8a22b80dd0f6c760cd3e432ebde888c"},
                {"taskId": "final-bar-stage1-reseat", "title": "Reseat the final conformance bar on the stage 1 contracts", "pullRequest": "local://mecattaf/tally.nix/tally/epsilon-campaign-019ffc34-90ba-7a52-a907-a539c01f74d7/final-bar-stage1-reseat-822864d640c0c6d5", "mergeCommit": "6afee3aa4b0eb1a8501876c90ba88197cd2a36dd"}
            ],
            "checkpoints": [{
                "taskId": "chapter-gate",
                "title": "Epsilon stage 1 gate: full gate ladder plus the final conformance bar",
                "revision": "6afee3aa4b0eb1a8501876c90ba88197cd2a36dd"
            }],
            "blocked": [],
            "outstanding": [],
            "steering": [],
            "retries": [],
            "deferrals": [],
            "warnings": [
                "campaign pardon local://campaign/epsilon/attempt-receipts/4 pardoned 3 earlier machine receipt(s) for task(s) 'chapter-gate'",
                "campaign pardon local://campaign/epsilon/attempt-receipts/7 pardoned 2 earlier machine receipt(s)",
                "campaign pardon local://campaign/epsilon/attempt-receipts/10 pardoned 2 earlier machine receipt(s)",
                "campaign pardon local://campaign/epsilon/attempt-receipts/15 pardoned 4 earlier machine receipt(s)",
                "campaign pardon local://campaign/epsilon/attempt-receipts/18 pardoned 2 earlier machine receipt(s)"
            ]
        }),
        expected_summary: STAGE_ONE_COMPLETE_SUMMARY,
    }
}
