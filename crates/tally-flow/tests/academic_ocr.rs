use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use tally_flow::{
    run_script, Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission,
    NodeResult, RunInspection, RunOptions, VecLifecycleSink, Verdict,
};

const SOURCE: &str = include_str!("../../../examples/flows/academic-ocr.js");

struct StubProtocolClient {
    submissions: RefCell<Vec<FlowSubmission>>,
}

impl StubProtocolClient {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            submissions: RefCell::default(),
        })
    }

    fn recognition_result(brief: &Value) -> Value {
        let page = brief["page"]["pageNumber"].as_u64().expect("page number");
        let protocol = brief["protocol"]["id"].as_str().expect("protocol id");
        let variant = brief["input"]["id"].as_str().expect("input variant");
        let signature = match (page, variant, protocol) {
            (1, "rerasterize-400-dpi", "cheap-a" | "cheap-b") => vec![7, 7, 7],
            (1, "original", "cheap-a") => vec![1, 1, 1],
            (1, "original", "cheap-b") => vec![2, 2, 2],
            (1, "original", "specialist") => vec![3, 3, 3],
            (2, "original", "cheap-a") => vec![10, 10, 10],
            (2, "original", "cheap-b") => vec![20, 20, 20],
            (2, "original", "specialist") => vec![30, 30, 30],
            (2, "rerasterize-400-dpi", "cheap-a") => vec![40, 40, 40],
            (2, "rerasterize-400-dpi", "cheap-b") => vec![50, 50, 50],
            (2, "rerasterize-400-dpi", "specialist") => vec![60, 60, 60],
            unexpected => panic!("unexpected stub recognition {unexpected:?}"),
        };
        let digest_digit = match (page, variant, protocol) {
            (1, "rerasterize-400-dpi", "cheap-a" | "cheap-b") => 'a',
            (1, _, _) => 'b',
            (2, "rerasterize-400-dpi", _) => 'c',
            (2, _, _) => 'd',
            _ => unreachable!(),
        };
        json!({
            "paperId": brief["page"]["paperId"],
            "pageNumber": page,
            "protocolId": protocol,
            "inputVariant": variant,
            "artifactPath": brief["artifactPath"],
            "textDigest": format!("sha256:{}", digest_digit.to_string().repeat(64)),
            "signature": signature,
            "confidencePermille": if protocol == "cheap-b" { 900 } else { 800 },
            "hotZones": [{
                "x": page * 10,
                "y": 20,
                "width": 30,
                "height": 40
            }],
            "skewMilliDegrees": if page == 1 { 1200 } else { -900 }
        })
    }

    fn arbiter_result(brief: &Value) -> Value {
        let basis = brief["attempts"]
            .as_array()
            .expect("arbiter attempts")
            .iter()
            .filter_map(|attempt| attempt["artifactPath"].as_str())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        json!({
            "paperId": brief["page"]["paperId"],
            "pageNumber": brief["page"]["pageNumber"],
            "artifactPath": brief["artifactPath"],
            "textDigest": format!("sha256:{}", "e".repeat(64)),
            "basis": basis
        })
    }
}

impl FlowClient for StubProtocolClient {
    fn inspect_run<'a>(
        &'a self,
        _flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
        Box::pin(std::future::ready(Ok(RunInspection::default())))
    }

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>> {
        let index = self.submissions.borrow().len();
        let brief = submission.spec.brief.as_ref().expect("structured brief");
        let result = match brief["action"].as_str().expect("driver action") {
            "recognize" => Self::recognition_result(brief),
            "arbitrate" => Self::arbiter_result(brief),
            action => panic!("unexpected stub action {action:?}"),
        };
        let task_uuid = format!("ocr-stub-{index}");
        let task_ref = submission.orchestration.task_ref.clone();
        let terminal = NodeResult {
            task_uuid: task_uuid.clone(),
            task_ref: task_ref.clone(),
            verdict: Verdict::Pass,
            exit_code: Some(0),
            witness_seq: u64::try_from(index + 1).expect("test ordinal fits u64"),
            disposition: Disposition::Created,
            result: Some(result),
            gates: None,
            error: None,
        };
        let payload_hash = submission.payload_hash.clone();
        self.submissions.borrow_mut().push(submission);
        Box::pin(std::future::ready(Ok(Admission {
            schema_version: 1,
            disposition: Disposition::Created,
            task_uuid,
            task_ref,
            payload_hash,
            attempt: 1,
            terminal: Some(terminal),
            recorded_label: None,
            reused_rejected: None,
        })))
    }

    fn await_terminal<'a>(
        &'a self,
        task_uuid: &'a str,
        _attempt: u32,
    ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
        Box::pin(std::future::ready(Err(ClientError::new(
            "unexpected-await",
            task_uuid,
        ))))
    }
}

fn options() -> RunOptions {
    let mut options = RunOptions::new(
        "academic-ocr-stub-run",
        json!({
            "pages": [
                {
                    "paperId": "paper-a",
                    "pageNumber": 2,
                    "sourcePath": "/fixtures/paper-a/page-2.pdf"
                },
                {
                    "paperId": "paper-a",
                    "pageNumber": 1,
                    "sourcePath": "/fixtures/paper-a/page-1.pdf"
                }
            ],
            "protocols": [
                { "id": "specialist", "tier": "specialist" },
                { "id": "cheap-b", "tier": "cheap" },
                { "id": "cheap-a", "tier": "cheap" }
            ],
            "driver": {
                "adapter": "stub-ocr",
                "program": "/nix/store/00000000000000000000000000000000-stub-ocr/bin/stub-ocr",
                "runtimeMaxSec": 60
            },
            "outputDir": "/tmp/academic-ocr",
            "rasterDpi": 400,
            "maxMutationIterations": 1,
            "maxDisagreementPermille": 100
        }),
    );
    options.max_nodes = 1700;
    options
}

#[test]
fn reduced_swarm_runs_one_mutation_and_one_arbiter_with_stub_protocols() {
    let client = StubProtocolClient::new();
    let report = run_script(
        SOURCE,
        Some(Path::new("examples/flows/academic-ocr.js")),
        client.clone(),
        Rc::new(VecLifecycleSink::default()),
        options(),
    )
    .expect("reduced OCR swarm succeeds");
    let final_value = report.final_value.expect("flow final value");

    assert_eq!(final_value["pageCount"], 2);
    assert_eq!(final_value["convergedCount"], 1);
    assert_eq!(final_value["arbitratedCount"], 1);
    assert_eq!(final_value["configuredNodeUpperBound"], 14);
    assert_eq!(final_value["pages"][0]["pageNumber"], 1);
    assert_eq!(final_value["pages"][0]["resolution"], "mutation");
    assert_eq!(
        final_value["pages"][0]["inputVariant"],
        "rerasterize-400-dpi"
    );
    assert_eq!(final_value["pages"][0]["attemptCount"], 5);
    assert_eq!(
        final_value["pages"][0]["chosenArtifactPath"],
        "/tmp/academic-ocr/paper-a/page-1/cheap-b/rerasterize-400-dpi.json"
    );
    assert_eq!(final_value["pages"][1]["pageNumber"], 2);
    assert_eq!(final_value["pages"][1]["resolution"], "arbiter");
    assert_eq!(final_value["pages"][1]["attemptCount"], 6);
    assert_eq!(
        final_value["pages"][1]["chosenArtifactPath"],
        "/tmp/academic-ocr/paper-a/page-2/arbiter/final.json"
    );

    let submissions = client.submissions.borrow();
    assert_eq!(submissions.len(), 12);
    let labels = submissions
        .iter()
        .map(|submission| {
            submission
                .orchestration
                .node_label
                .as_deref()
                .expect("node label")
        })
        .collect::<Vec<_>>();
    assert!(labels.contains(&"ocr-paper-a-1-specialist-original"));
    assert!(labels.contains(&"ocr-paper-a-1-cheap-a-rerasterize-400-dpi"));
    assert!(!labels.contains(&"ocr-paper-a-1-specialist-rerasterize-400-dpi"));
    assert!(labels.contains(&"ocr-paper-a-2-specialist-rerasterize-400-dpi"));
    assert!(labels.contains(&"ocr-paper-a-2-arbiter"));

    let keys = submissions
        .iter()
        .map(|submission| submission.dedup_key.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(keys.len(), submissions.len());
    for submission in submissions.iter() {
        assert_eq!(submission.spec.pools, ["ocr-gpu"]);
        assert_eq!(submission.spec.priority.as_deref(), Some("low"));
        assert_eq!(
            submission.spec.evidence.last().map(String::as_str),
            Some("hash:sha256")
        );
        assert!(submission.spec.result_schema.is_some());
    }
    let arbiter = submissions
        .iter()
        .find(|submission| {
            submission.orchestration.node_label.as_deref() == Some("ocr-paper-a-2-arbiter")
        })
        .expect("arbiter submission");
    assert_eq!(
        arbiter.spec.brief.as_ref().unwrap()["attempts"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
}
