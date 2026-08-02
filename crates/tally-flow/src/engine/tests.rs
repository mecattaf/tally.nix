use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};

use super::*;
use crate::{Admission, CatalogMember, ClientError, FlowFuture, RunInspection, TaskRef, Verdict};

#[derive(Clone)]
struct Reply {
    disposition: Disposition,
    witness_seq: u64,
    verdict: Verdict,
    stderr_excerpt: Option<String>,
    stderr_truncated: Option<bool>,
    result: Option<Value>,
    divergent_hash: bool,
    client_error: Option<ClientError>,
}

impl Reply {
    fn pass(disposition: Disposition, witness_seq: u64) -> Self {
        Self {
            disposition,
            witness_seq,
            verdict: Verdict::Pass,
            stderr_excerpt: None,
            stderr_truncated: None,
            result: Some(json!({"ok": true})),
            divergent_hash: false,
            client_error: None,
        }
    }

    fn client_error(code: &str) -> Self {
        Self {
            disposition: Disposition::Created,
            witness_seq: 1,
            verdict: Verdict::Failed,
            stderr_excerpt: None,
            stderr_truncated: None,
            result: None,
            divergent_hash: false,
            client_error: Some(ClientError::new(code, format!("{code} from mock"))),
        }
    }
}

struct MockClient {
    inspection: RunInspection,
    replies: RefCell<VecDeque<Reply>>,
    submissions: RefCell<Vec<FlowSubmission>>,
    terminals: RefCell<BTreeMap<String, NodeResult>>,
    delayed_submissions: RefCell<BTreeSet<usize>>,
    hang_submissions: bool,
}

impl MockClient {
    fn new(replies: Vec<Reply>) -> Rc<Self> {
        Rc::new(Self {
            inspection: RunInspection::default(),
            replies: RefCell::new(replies.into()),
            submissions: RefCell::default(),
            terminals: RefCell::default(),
            delayed_submissions: RefCell::default(),
            hang_submissions: false,
        })
    }

    fn with_script_hash(hash: &str) -> Rc<Self> {
        Rc::new(Self {
            inspection: RunInspection {
                script_hash: Some(hash.to_owned()),
                ..RunInspection::default()
            },
            replies: RefCell::default(),
            submissions: RefCell::default(),
            terminals: RefCell::default(),
            delayed_submissions: RefCell::default(),
            hang_submissions: false,
        })
    }

    fn with_inspection(inspection: RunInspection) -> Rc<Self> {
        Rc::new(Self {
            inspection,
            replies: RefCell::default(),
            submissions: RefCell::default(),
            terminals: RefCell::default(),
            delayed_submissions: RefCell::default(),
            hang_submissions: false,
        })
    }

    fn hanging() -> Rc<Self> {
        Rc::new(Self {
            inspection: RunInspection::default(),
            replies: RefCell::default(),
            submissions: RefCell::default(),
            terminals: RefCell::default(),
            delayed_submissions: RefCell::default(),
            hang_submissions: true,
        })
    }

    fn delay_submission(&self, ordinal: usize) {
        self.delayed_submissions.borrow_mut().insert(ordinal);
    }
}

impl FlowClient for MockClient {
    fn inspect_run<'a>(
        &'a self,
        _flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
        Box::pin(std::future::ready(Ok(self.inspection.clone())))
    }

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>> {
        if self.hang_submissions {
            self.submissions.borrow_mut().push(submission);
            return Box::pin(std::future::pending());
        }
        let index = self.submissions.borrow().len();
        let reply = self
            .replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Reply::pass(Disposition::Created, (index + 1) as u64));
        if let Some(error) = reply.client_error {
            self.submissions.borrow_mut().push(submission);
            return Box::pin(std::future::ready(Err(error)));
        }
        let task_uuid = submission
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("task-{index}"));
        let task_ref = submission.orchestration.task_ref.clone();
        let terminal = NodeResult {
            task_uuid: task_uuid.clone(),
            task_ref: task_ref.clone(),
            verdict: reply.verdict,
            exit_code: Some(if reply.verdict.is_pass() { 0 } else { 1 }),
            stderr_excerpt: reply.stderr_excerpt,
            stderr_truncated: reply.stderr_truncated,
            witness_seq: reply.witness_seq,
            disposition: reply.disposition,
            model: None,
            result: reply.result,
            gates: None,
            error: (!reply.verdict.is_pass()).then(|| NodeFailure {
                code: "worker-failed".to_owned(),
                message: "worker failed".to_owned(),
                details: None,
            }),
        };
        let inline = matches!(
            reply.disposition,
            Disposition::Reused | Disposition::Substituted | Disposition::Terminal
        )
        .then(|| terminal.clone());
        self.terminals
            .borrow_mut()
            .insert(task_uuid.clone(), terminal);
        let payload_hash = if reply.divergent_hash {
            "sha256:divergent".to_owned()
        } else {
            submission.payload_hash.clone()
        };
        self.submissions.borrow_mut().push(submission);
        let mut admission = Some(Ok(Admission {
            schema_version: 1,
            disposition: reply.disposition,
            task_uuid,
            task_ref,
            payload_hash,
            attempt: 0,
            terminal: inline,
            recorded_label: None,
            reused_rejected: None,
        }));
        let delayed = self.delayed_submissions.borrow().contains(&index);
        let mut yielded = false;
        Box::pin(std::future::poll_fn(move |cx| {
            if delayed && !yielded {
                yielded = true;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            Poll::Ready(
                admission
                    .take()
                    .expect("mock submission future is not polled after completion"),
            )
        }))
    }

    fn await_terminal<'a>(
        &'a self,
        task_uuid: &'a str,
        _attempt: u32,
    ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
        let result = self
            .terminals
            .borrow()
            .get(task_uuid)
            .cloned()
            .ok_or_else(|| ClientError::new("missing-terminal", task_uuid));
        Box::pin(std::future::ready(result))
    }
}

#[derive(Clone)]
struct DurableNode {
    ordinal: u64,
    task_uuid: String,
    payload_hash: String,
    label: Option<String>,
    task_ref: Option<TaskRef>,
    script_hash: String,
    args_hash: String,
    catalog_hash: Option<String>,
    terminal: Option<NodeResult>,
}

struct BudgetContinuationClient {
    pause_first_evaluation_at: Option<u64>,
    evaluation: Cell<u32>,
    nodes: RefCell<BTreeMap<String, DurableNode>>,
    admissions: RefCell<Vec<(u32, u64, Disposition)>>,
    executions: RefCell<Vec<u64>>,
}

impl BudgetContinuationClient {
    fn new(pause_first_evaluation_at: Option<u64>) -> Rc<Self> {
        Rc::new(Self {
            pause_first_evaluation_at,
            evaluation: Cell::new(0),
            nodes: RefCell::default(),
            admissions: RefCell::default(),
            executions: RefCell::default(),
        })
    }

    fn terminal(node: &DurableNode, disposition: Disposition) -> NodeResult {
        NodeResult {
            task_uuid: node.task_uuid.clone(),
            task_ref: node.task_ref.clone(),
            verdict: Verdict::Pass,
            exit_code: Some(0),
            stderr_excerpt: None,
            stderr_truncated: None,
            witness_seq: node.ordinal + 1,
            disposition,
            model: None,
            result: Some(json!({"step": node.ordinal + 1})),
            gates: None,
            error: None,
        }
    }
}

impl FlowClient for BudgetContinuationClient {
    fn inspect_run<'a>(
        &'a self,
        _flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
        self.evaluation.set(self.evaluation.get() + 1);
        let inspection =
            self.nodes
                .borrow()
                .values()
                .next()
                .map_or_else(RunInspection::default, |node| RunInspection {
                    script_hash: Some(node.script_hash.clone()),
                    args_hash: Some(node.args_hash.clone()),
                    catalog_hash: node.catalog_hash.clone(),
                });
        Box::pin(std::future::ready(Ok(inspection)))
    }

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>> {
        let evaluation = self.evaluation.get();
        let ordinal = submission.orchestration.node_ordinal;
        let dedup_key = submission.dedup_key.clone();
        let mut nodes = self.nodes.borrow_mut();

        let admission = if let Some(node) = nodes.get(&dedup_key) {
            assert_eq!(submission.payload_hash, node.payload_hash);
            let disposition = if node.terminal.is_some() {
                Disposition::Reused
            } else {
                Disposition::Attached
            };
            Admission {
                schema_version: 1,
                disposition,
                task_uuid: node.task_uuid.clone(),
                task_ref: node.task_ref.clone(),
                payload_hash: node.payload_hash.clone(),
                attempt: 0,
                terminal: node
                    .terminal
                    .as_ref()
                    .map(|_| Self::terminal(node, disposition)),
                recorded_label: node.label.clone(),
                reused_rejected: None,
            }
        } else {
            let mut node = DurableNode {
                ordinal,
                task_uuid: format!("task-{ordinal}"),
                payload_hash: submission.payload_hash.clone(),
                label: submission.spec.label.clone(),
                task_ref: submission.spec.task_ref.clone(),
                script_hash: submission.orchestration.script_hash.clone(),
                args_hash: submission.orchestration.args_hash.clone(),
                catalog_hash: submission.orchestration.catalog_hash.clone(),
                terminal: None,
            };
            self.executions.borrow_mut().push(ordinal);
            if !(evaluation == 1 && self.pause_first_evaluation_at == Some(ordinal)) {
                node.terminal = Some(Self::terminal(&node, Disposition::Created));
            }
            let admission = Admission {
                schema_version: 1,
                disposition: Disposition::Created,
                task_uuid: node.task_uuid.clone(),
                task_ref: node.task_ref.clone(),
                payload_hash: node.payload_hash.clone(),
                attempt: 0,
                terminal: None,
                recorded_label: None,
                reused_rejected: None,
            };
            nodes.insert(dedup_key, node);
            admission
        };
        self.admissions
            .borrow_mut()
            .push((evaluation, ordinal, admission.disposition));
        Box::pin(std::future::ready(Ok(admission)))
    }

    fn await_terminal<'a>(
        &'a self,
        task_uuid: &'a str,
        _attempt: u32,
    ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
        let evaluation = self.evaluation.get();
        let mut nodes = self.nodes.borrow_mut();
        let Some(node) = nodes.values_mut().find(|node| node.task_uuid == task_uuid) else {
            return Box::pin(std::future::ready(Err(ClientError::new(
                "missing-terminal",
                task_uuid,
            ))));
        };
        if evaluation == 1 && self.pause_first_evaluation_at == Some(node.ordinal) {
            return Box::pin(std::future::pending());
        }
        if node.terminal.is_none() {
            node.terminal = Some(Self::terminal(node, Disposition::Created));
        }
        Box::pin(std::future::ready(Ok(node
            .terminal
            .clone()
            .expect("continuation terminal was materialized"))))
    }
}

fn meta(pools: &[&str], selectors: &[&str]) -> String {
    format!(
        "export const meta = {{\n\
         name: 'test-flow',\n\
         description: 'engine test',\n\
         pools: {},\n\
         argsSchema: {{type: 'object'}},\n\
         selectors: {}\n\
         }};\n",
        serde_json::to_string(pools).unwrap(),
        serde_json::to_string(selectors).unwrap(),
    )
}

fn run(
    source: &str,
    client: Rc<MockClient>,
) -> Result<(RunReport, Rc<VecLifecycleSink>), FlowError> {
    let sink = Rc::new(VecLifecycleSink::default());
    let report = run_script(
        source,
        Some(Path::new("test-flow.js")),
        client,
        sink.clone(),
        RunOptions::new("run-1", json!({})),
    )?;
    Ok((report, sink))
}

#[test]
fn parallel_repeats_an_identical_ordinal_stream_three_times() {
    let source = format!(
        "{}\n(async () => parallel([\n\
         () => sh(['one'], {{pools: ['cpu'], label: 'one'}}),\n\
         () => sh(['two'], {{pools: ['cpu'], label: 'two'}}),\n\
         () => sh(['three'], {{pools: ['cpu'], label: 'three'}})\n\
         ]))()",
        meta(&["cpu"], &[])
    );
    let mut streams = Vec::new();
    for _ in 0..3 {
        let client = MockClient::new(Vec::new());
        let (report, _) = run(&source, client.clone()).unwrap();
        streams.push((
            report.ordinal_keys,
            client
                .submissions
                .borrow()
                .iter()
                .map(|submission| {
                    (
                        submission.orchestration.node_ordinal,
                        submission.dedup_key.clone(),
                        submission.payload_hash.clone(),
                    )
                })
                .collect::<Vec<_>>(),
        ));
    }
    assert_eq!(streams[0], streams[1]);
    assert_eq!(streams[1], streams[2]);
    assert_eq!(
        streams[0].0,
        ["flow:run-1:0", "flow:run-1:1", "flow:run-1:2"]
    );
}

#[test]
fn task_ref_becomes_orchestration_and_self_describes_both_lifecycle_events() {
    let source = format!(
        "{}\n(async () => sh(['ship'], {{pools: ['cpu'], taskRef: 'crm/t07'}}))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(Vec::new());
    let (_, sink) = run(&source, client.clone()).unwrap();
    let submissions = client.submissions.borrow();
    assert_eq!(
        submissions[0]
            .orchestration
            .task_ref
            .as_ref()
            .map(TaskRef::as_str),
        Some("crm/t07")
    );
    assert_eq!(
        serde_json::to_value(&submissions[0].orchestration).unwrap()["taskRef"],
        "crm/t07"
    );
    let node_events = sink
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                event["type"].as_str(),
                Some("node-submitted" | "node-terminal")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(node_events.len(), 2);
    assert!(node_events
        .iter()
        .all(|event| event["taskRef"] == "crm/t07"));
}

#[test]
fn pipeline_advances_each_item_without_a_stage_barrier() {
    let source = format!(
        "{}\n(async () => pipeline(['a', 'b'],\n\
         (_previous, item) => sh([item, 'stage-1'], {{pools: ['cpu']}}),\n\
         (_previous, item) => sh([item, 'stage-2'], {{pools: ['cpu']}})\n\
         ))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Created, 1),
        Reply::pass(Disposition::Created, 4),
        Reply::pass(Disposition::Created, 2),
        Reply::pass(Disposition::Created, 5),
    ]);
    let (report, _) = run(&source, client.clone()).unwrap();
    assert_eq!(report.observation_order, [1, 2, 4, 5]);
    assert_eq!(
        client
            .submissions
            .borrow()
            .iter()
            .map(|submission| submission.spec.argv.clone().unwrap())
            .collect::<Vec<_>>(),
        [
            ["a", "stage-1"],
            ["b", "stage-1"],
            ["a", "stage-2"],
            ["b", "stage-2"],
        ]
    );
}

#[test]
fn every_disposition_observes_in_witness_order_and_suppresses_prefix_logs() {
    let source = format!(
        "{}\n(async () => {{\n\
         log('frontier-0');\n\
         const values = await parallel([\n\
         () => sh(['zero'], {{pools: ['cpu']}}),\n\
         () => sh(['one'], {{pools: ['cpu']}}),\n\
         () => sh(['two'], {{pools: ['cpu']}}),\n\
         () => sh(['three'], {{pools: ['cpu']}})\n\
         ]);\n\
         log('tail');\n\
         return values.map(value => value.witnessSeq);\n\
         }})()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Reused, 30),
        Reply::pass(Disposition::Terminal, 10),
        Reply::pass(Disposition::Attached, 20),
        Reply::pass(Disposition::Created, 40),
    ]);
    client.delay_submission(1);
    let (report, sink) = run(&source, client).unwrap();
    assert_eq!(report.observation_order, [10, 20, 30, 40]);
    assert_eq!(report.final_value, Some(json!([30, 10, 20, 40])));
    let logs = sink
        .events()
        .into_iter()
        .filter(|event| event["type"] == "log")
        .collect::<Vec<_>>();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["message"], "tail");
}

#[test]
fn payload_divergence_stops_admission_at_the_mismatched_ordinal() {
    let source = format!(
        "{}\n(async () => parallel([\n\
         () => sh(['zero'], {{pools: ['cpu']}}),\n\
         () => sh(['one'], {{pools: ['cpu']}}),\n\
         () => sh(['two'], {{pools: ['cpu']}})\n\
         ]))()",
        meta(&["cpu"], &[])
    );
    let mut mismatch = Reply::pass(Disposition::Reused, 2);
    mismatch.divergent_hash = true;
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Reused, 1),
        mismatch,
        Reply::pass(Disposition::Created, 3),
    ]);
    let error = run(&source, client.clone()).unwrap_err();
    assert_eq!(error.name, "FlowReplayError");
    assert_eq!(error.code, "replay-divergence");
    assert_eq!(error.ordinal, Some(1));
    assert_eq!(
        error.details.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "expectedHash",
            "recordedHash",
            "expectedLabel",
            "recordedLabel"
        ]
    );
    assert_eq!(client.submissions.borrow().len(), 2);
}

#[test]
fn created_payload_mismatch_reports_contract_drift_on_first_admission() {
    let source = format!(
        "{}\n(async () => sh(['first'], {{pools: ['cpu'], label: 'first-node'}}))()",
        meta(&["cpu"], &[])
    );
    let mut mismatch = Reply::pass(Disposition::Created, 1);
    mismatch.divergent_hash = true;
    let client = MockClient::new(vec![mismatch]);

    let error = run(&source, client.clone()).unwrap_err();

    assert_eq!(error.name, "FlowContractError");
    assert_eq!(error.code, "payload-hash-contract-drift");
    assert_eq!(error.ordinal, Some(0));
    assert_eq!(error.details["recordedHash"], "sha256:divergent");
    assert!(error.details["expectedHash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(error.details["label"], "first-node");
    assert_eq!(client.submissions.borrow().len(), 1);
}

#[test]
fn a_flow_run_refuses_a_changed_script_before_submission() {
    let source = format!("{}\n42;", meta(&["cpu"], &[]));
    let client = MockClient::with_script_hash("sha256:previous-script");
    let error = run(&source, client.clone()).unwrap_err();
    assert_eq!(error.name, "FlowReplayError");
    assert_eq!(error.code, "script-changed-mid-run");
    assert!(error.location.is_some());
    assert!(client.submissions.borrow().is_empty());

    let invalid_edit = "export const meta = {";
    let client = MockClient::with_script_hash("sha256:previous-script");
    let error = run(invalid_edit, client.clone()).unwrap_err();
    assert_eq!(error.code, "script-changed-mid-run");
    assert!(client.submissions.borrow().is_empty());
}

#[test]
fn a_flow_run_pins_args_then_catalog_before_submission() {
    let source = format!("{}\n42;", meta(&["cpu"], &[]));
    let script_hash = sha256(source.as_bytes());
    let args = json!({"subject": "current"});
    let args_hash = sha256(&serde_json::to_vec(&args).unwrap());

    let args_client = MockClient::with_inspection(RunInspection {
        script_hash: Some(script_hash.clone()),
        args_hash: Some("sha256:previous-args".to_owned()),
        catalog_hash: None,
    });
    let error = run_script(
        &source,
        Some(Path::new("test-flow.js")),
        args_client.clone(),
        Rc::new(VecLifecycleSink::default()),
        RunOptions::new("run-1", args.clone()),
    )
    .unwrap_err();
    assert_eq!(error.name, "FlowReplayError");
    assert_eq!(error.code, "args-changed-mid-run");
    assert!(args_client.submissions.borrow().is_empty());

    let catalog_client = MockClient::with_inspection(RunInspection {
        script_hash: Some(script_hash),
        args_hash: Some(args_hash),
        catalog_hash: Some("sha256:previous-catalog".to_owned()),
    });
    let mut options = RunOptions::new("run-1", args);
    options.catalog = Some(catalog());
    options.catalog_hash = Some("sha256:current-catalog".to_owned());
    let error = run_script(
        &source,
        Some(Path::new("test-flow.js")),
        catalog_client.clone(),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap_err();
    assert_eq!(error.name, "FlowReplayError");
    assert_eq!(error.code, "catalog-changed-mid-run");
    assert!(catalog_client.submissions.borrow().is_empty());
}

#[test]
fn a_concurrent_identity_change_stops_the_admission_frontier() {
    let source = format!(
        "{}\n(async () => parallel([\n\
         () => sh(['one'], {{pools: ['cpu'], settle: true}}),\n\
         () => sh(['two'], {{pools: ['cpu'], settle: true}})\n\
         ]))()",
        meta(&["cpu"], &[])
    );
    for code in [
        "script-changed-mid-run",
        "args-changed-mid-run",
        "catalog-changed-mid-run",
    ] {
        let client = MockClient::new(vec![Reply::client_error(code)]);
        let error = run(&source, client.clone()).unwrap_err();
        assert_eq!(error.name, "FlowReplayError");
        assert_eq!(error.code, code);
        assert_eq!(client.submissions.borrow().len(), 1);
    }
}

#[test]
fn args_and_catalog_pins_do_not_enter_the_canonical_payload_hash() {
    let source = format!(
        "{}\n(async () => sh(['fixed'], {{pools: ['cpu']}}))()",
        meta(&["cpu"], &[])
    );
    let cases = [
        (json!({"alpha": 1, "beta": 2}), None),
        (json!({"alpha": 9, "beta": 2}), None),
        (
            json!({"alpha": 1, "beta": 2}),
            Some("sha256:catalog-generation"),
        ),
    ];
    let mut identities = Vec::new();
    for (args, catalog_hash) in cases {
        let client = MockClient::new(Vec::new());
        let mut options = RunOptions::new("run-1", args);
        if let Some(catalog_hash) = catalog_hash {
            options.catalog = Some(catalog());
            options.catalog_hash = Some(catalog_hash.to_owned());
        }
        run_script(
            &source,
            Some(Path::new("identity-pins.js")),
            client.clone(),
            Rc::new(VecLifecycleSink::default()),
            options,
        )
        .unwrap();
        let submission = client.submissions.borrow()[0].clone();
        let orchestration = serde_json::to_value(&submission.orchestration).unwrap();
        identities.push((
            submission.payload_hash,
            submission.orchestration.args_hash,
            submission.orchestration.catalog_hash,
            orchestration,
        ));
    }
    assert_eq!(identities[0].0, identities[1].0);
    assert_eq!(identities[0].0, identities[2].0);
    assert_ne!(identities[0].1, identities[1].1);
    assert_eq!(identities[0].1, identities[2].1);
    assert_eq!(identities[0].2, None);
    assert_eq!(
        identities[2].2.as_deref(),
        Some("sha256:catalog-generation")
    );
    assert!(identities[0].3["catalogHash"].is_null());
    assert_eq!(
        identities[0].1,
        "sha256:955c071f4fbee40a01b9bc6e8fb3627e81bda84811ae9c29fcc5812ba3a45162"
    );
}

#[test]
fn result_schema_handles_payloads_larger_than_sixty_four_kibibytes() {
    let payload = "x".repeat(70 * 1024);
    let source = format!(
        "{}\n(async () => {{\n\
         const node = await sh(['large'], {{\n\
           pools: ['cpu'],\n\
           resultSchema: {{type: 'object', required: ['payload'], properties: {{payload: {{type: 'string'}}}}}}\n\
         }});\n\
         return node.result.payload.length;\n\
         }})()",
        meta(&["cpu"], &[])
    );
    let mut reply = Reply::pass(Disposition::Created, 1);
    reply.result = Some(json!({"payload": payload}));
    let (report, _) = run(&source, MockClient::new(vec![reply])).unwrap();
    assert_eq!(report.final_value, Some(json!(70 * 1024)));
}

#[test]
fn duplicate_keys_and_result_mismatches_are_typed_with_positions() {
    let duplicate = format!(
        "{}\n(async () => {{\n\
         sh(['a'], {{pools: ['cpu'], key: 'same'}});\n\
         sh(['b'], {{pools: ['cpu'], key: 'same'}});\n\
         }})()",
        meta(&["cpu"], &[])
    );
    let error = run(&duplicate, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowKeyError");
    assert_eq!(error.code, "duplicate-key");
    let duplicate_line = duplicate
        .lines()
        .position(|line| line.contains("sh(['b']"))
        .unwrap();
    let duplicate_column = duplicate
        .lines()
        .nth(duplicate_line)
        .unwrap()
        .find("sh(")
        .unwrap();
    assert_eq!(
        error.location,
        Some(SourceLocation::new(
            duplicate_line as u32 + 1,
            duplicate_column as u32 + 1
        ))
    );

    let mismatch = format!(
        "{}\n(async () => sh(['bad'], {{\n\
         pools: ['cpu'], resultSchema: {{type: 'number'}}\n\
         }}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&mismatch, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "result-schema-mismatch");
    assert!(error.location.is_some());

    let settled = format!(
        "{}\n(async () => {{\n\
         const node = await sh(['bad'], {{\n\
           pools: ['cpu'], resultSchema: {{type: 'number'}}, settle: true\n\
         }});\n\
         return node.error.code;\n\
         }})()",
        meta(&["cpu"], &[])
    );
    let (report, _) = run(&settled, MockClient::new(Vec::new())).unwrap();
    assert_eq!(
        report.final_value,
        Some(Value::String("result-schema-mismatch".to_owned()))
    );
}

fn catalog() -> Catalog {
    Catalog {
        version: 1,
        members: ["alpha", "beta", "gamma"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| CatalogMember {
                id: id.to_owned(),
                family: format!("family-{index}"),
                maker: format!("maker-{index}"),
                classes: vec!["pooled".to_owned()],
                adapter: "pi".to_owned(),
                pools: vec!["gpu".to_owned()],
                launch: json!({"model": id}),
                architecture: None,
                fine_tune: None,
                backend: None,
                modality: None,
                role: None,
                status: None,
                evidence: None,
                hosts: Vec::new(),
                base_checkpoint: None,
                supersedes: None,
                superseded_by: None,
                notes: None,
            })
            .collect(),
    }
}

#[test]
fn local_unknown_options_use_the_shared_node_field_error_shape() {
    let source = format!(
        "{}\n(async () => {{\n\
         const member = members('pooled', {{count: 1}})[0];\n\
         return local('mission', {{member, resultShema: {{type: 'object'}}}});\n\
         }})()",
        meta(&["gpu"], &["pooled"])
    );
    let mut options = RunOptions::new("run-1", json!({}));
    options.catalog = Some(catalog());
    options.catalog_hash = Some("sha256:catalog".to_owned());
    let error = run_script(
        &source,
        Some(Path::new("local-unknown.js")),
        MockClient::new(Vec::new()),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap_err();
    assert_eq!(error.name, "FlowSpecError");
    assert_eq!(error.code, "unknown-spec-field");
    assert_eq!(
        error.message,
        "unknown local() option \"resultShema\", expected one of argv, adapter, pools, executor, \
         priority, runtimeMaxSec, evidence, evidenceClass, manifestHash, workspace, brief, key, \
         dedupKey, label, taskRef, env, approvalPolicy, sandboxPolicy, model, resultSchema, member"
    );
    assert_eq!(error.details["field"], "resultShema");
}

#[test]
fn selector_quorum_preserves_dissent_and_materializes_one_repair_key() {
    let source = format!(
        "{}\n(async () => {{\n\
         const selected = members('pooled', {{count: 3, diversity: 'maker'}});\n\
         const outcomes = await parallel(selected.map(member => () =>\n\
           local('judge', {{member, settle: true}})\n\
         ), {{settle: true}});\n\
         const attributedRows = outcomes.map((outcome, index) => attributed(selected[index], outcome));\n\
         const q = quorum({{\n\
           results: attributedRows,\n\
           minimumValid: 2,\n\
           requiredMembers: selected.map(member => member.id),\n\
           allowPartial: true\n\
         }});\n\
         const repair = await local('repair', {{member: selected[1], key: repairKey(selected[1])}});\n\
         return {{\n\
           q,\n\
           repair: repair.verdict,\n\
           dissent: dissent({{\n\
             conclusions: [{{conclusion: 'ship', support: ['alpha', 'gamma'], conflict: ['beta']}}],\n\
             excluded: [{{memberId: 'beta', reason: 'invalid'}}]\n\
           }})\n\
         }};\n\
         }})()",
        meta(&["gpu"], &["pooled"])
    );
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Created, 1),
        Reply {
            disposition: Disposition::Created,
            witness_seq: 2,
            verdict: Verdict::Failed,
            stderr_excerpt: None,
            stderr_truncated: None,
            result: None,
            divergent_hash: false,
            client_error: None,
        },
        Reply::pass(Disposition::Created, 3),
        Reply::pass(Disposition::Created, 4),
    ]);
    let sink = Rc::new(VecLifecycleSink::default());
    let mut options = RunOptions::new("run-1", json!({}));
    options.catalog = Some(catalog());
    options.catalog_hash = Some("sha256:catalog".to_owned());
    let report = run_script(
        &source,
        Some(Path::new("quorum.js")),
        client.clone(),
        sink.clone(),
        options,
    )
    .unwrap();
    assert_eq!(
        report.final_value.as_ref().unwrap()["q"]["valid"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        report.final_value.as_ref().unwrap()["dissent"]["conclusions"][0]["conflict"],
        json!(["beta"])
    );
    assert_eq!(
        client.submissions.borrow()[3].dedup_key,
        "flow:run-1:k:beta@1"
    );
    assert_eq!(
        sink.events()
            .iter()
            .position(|event| event["type"] == "selector-resolved"),
        Some(0)
    );
    assert!(client
        .submissions
        .borrow()
        .iter()
        .take(3)
        .all(|submission| {
            submission.orchestration.selection.is_some()
                && submission.orchestration.prompt_revision.is_some()
                && submission.orchestration.skill_revision.is_none()
        }));
}

#[test]
fn every_agent_sugar_carries_revision_and_prompt_only_in_the_structured_brief() {
    let source = format!(
        "{}\n(async () => {{\n\
         const member = members('pooled', {{count: 1}})[0];\n\
         return parallel([\n\
           () => claude('claude mission'),\n\
           () => codex('codex mission'),\n\
           () => local('local mission', {{member}}),\n\
           () => sh(['printf', 'ordinary argv'], {{pools: ['gpu']}})\n\
         ]);\n\
         }})()",
        meta(&["claude-window", "codex-window", "gpu"], &["pooled"])
    );
    let client = MockClient::new(Vec::new());
    let mut options = RunOptions::new("run-1", json!({}));
    options.catalog = Some(catalog());
    options.catalog_hash = Some("sha256:catalog".to_owned());
    options.adapter_skill_revisions = BTreeMap::from([
        ("claude-code".to_owned(), "claude-skill-v2".to_owned()),
        ("codex".to_owned(), "sha256:codex-skill-content".to_owned()),
        ("pi".to_owned(), "local-skill-v4".to_owned()),
    ]);
    run_script(
        &source,
        Some(Path::new("sugar.js")),
        client.clone(),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap();
    let submissions = client.submissions.borrow();
    for (submission, (mission, prompt_revision, skill_revision)) in
        submissions.iter().take(3).zip([
            (
                "claude mission",
                "sha256:fb26460aa413216cbc1ff6d4a4f1d248e88b54966fecdb18c220b3cdd46635bb",
                "claude-skill-v2",
            ),
            (
                "codex mission",
                "sha256:ee65191fbc19d66fba6d51c4350e3bfaeaf779afba302312680ef6ea5d1d664a",
                "sha256:codex-skill-content",
            ),
            (
                "local mission",
                "sha256:d07d2dee7383b022e8c9da1cb4767c119a0e31c7b03528a44f5b940524dc985e",
                "local-skill-v4",
            ),
        ])
    {
        assert_eq!(
            submission.spec.argv.as_deref(),
            Some(&[BRIEF_SENTINEL.to_owned()][..])
        );
        assert_eq!(submission.spec.brief, Some(json!({"mission": mission})));
        assert_eq!(
            submission.orchestration.prompt_revision.as_deref(),
            Some(prompt_revision)
        );
        assert_eq!(
            submission.orchestration.skill_revision.as_deref(),
            Some(skill_revision)
        );
        assert!(!submission
            .spec
            .argv
            .as_ref()
            .unwrap()
            .iter()
            .any(|argument| argument.contains(mission)));
    }
    assert_eq!(
        submissions[3].spec.argv.as_deref(),
        Some(&["printf".to_owned(), "ordinary argv".to_owned()][..])
    );
    assert_eq!(submissions[3].spec.adapter.as_deref(), Some("shell"));
    assert!(submissions[3].spec.brief.is_none());
    assert!(submissions[3].orchestration.prompt_revision.is_none());
    assert!(submissions[3].orchestration.skill_revision.is_none());
}

#[test]
fn drv_sugar_is_store_native_replay_stable_and_substituted_is_success() {
    const DRV: &str = "/nix/store/00000000000000000000000000000000-fixture.drv";
    const DEV: &str = "/nix/store/11111111111111111111111111111111-fixture-dev";
    const OUT: &str = "/nix/store/22222222222222222222222222222222-fixture";
    let source = format!(
        "{}\n(async () => drv({{\n\
         drvPath: {DRV:?},\n\
         outputs: [\n\
           {{name: 'out', path: {OUT:?}}},\n\
           {{name: 'dev', path: {DEV:?}}}\n\
         ]\n\
         }}))()",
        meta(&[], &[])
    );
    let mut identities = Vec::new();
    for _ in 0..2 {
        let client = MockClient::new(vec![Reply {
            disposition: Disposition::Substituted,
            witness_seq: 7,
            verdict: Verdict::Substituted,
            stderr_excerpt: None,
            stderr_truncated: None,
            result: None,
            divergent_hash: false,
            client_error: None,
        }]);
        let (report, _) = run(&source, client.clone()).unwrap();
        assert_eq!(
            report.final_value.as_ref().unwrap()["disposition"],
            "substituted"
        );
        assert_eq!(
            report.final_value.as_ref().unwrap()["verdict"],
            "substituted"
        );
        let submission = client.submissions.borrow()[0].clone();
        assert_eq!(submission.dedup_key, format!("drv:{DRV}"));
        assert_eq!(
            submission.task_uuid.as_deref(),
            Some("35c1f3a2-0ec5-53bf-8019-62ac60ca5bb0")
        );
        assert_eq!(submission.spec.pools, ["build"]);
        assert_eq!(submission.spec.adapter.as_deref(), Some("shell"));
        assert_eq!(
            submission.spec.argv.as_deref(),
            Some(
                &[
                    "nix".to_owned(),
                    "build".to_owned(),
                    "--no-link".to_owned(),
                    format!("{DRV}^*")
                ][..]
            )
        );
        assert_eq!(
            submission.spec.evidence,
            [format!("store:{DEV}"), format!("store:{OUT}")]
        );
        assert_eq!(
            submission
                .spec
                .drv
                .as_ref()
                .unwrap()
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            ["dev", "out"]
        );
        assert_eq!(
            submission.payload_hash,
            "sha256:7420a9161793b05545bbb806bf1449a9554f756b8e4d800718050b6447b31f7f"
        );
        identities.push((submission.task_uuid, submission.payload_hash));
    }
    assert_eq!(identities[0], identities[1]);
}

#[test]
fn drv_sugar_rejects_noncanonical_derivations_before_submission() {
    let cases = [
        (
            "{drvPath: '/tmp/not-a.drv', outputs: [{name: 'out', path: '/nix/store/11111111111111111111111111111111-out'}]}",
            "drvPath must be a Nix store path ending in .drv",
        ),
        (
            "{drvPath: '/nix/store/00000000000000000000000000000000-empty.drv', outputs: []}",
            "drv outputs must be non-empty",
        ),
        (
            "{drvPath: '/nix/store/00000000000000000000000000000000-dupe.drv', outputs: [{name: 'out', path: '/nix/store/11111111111111111111111111111111-one'}, {name: 'out', path: '/nix/store/22222222222222222222222222222222-two'}]}",
            "drv outputs must be sorted by name and unique",
        ),
    ];
    for (spec, message) in cases {
        let source = format!("{}\ndrv({spec});", meta(&[], &[]));
        let error = run(&source, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.code, "invalid-derivation");
        assert!(error.message.contains(message), "{error}");
        assert!(error.location.is_some());
    }
}

#[test]
fn resolved_prompt_revision_and_payload_identity_are_replay_stable() {
    let source = format!(
        "{}\n(async () => claude('resolved ' + args.suffix))()",
        meta(&["claude-window"], &[])
    );
    let mut streams = Vec::new();
    for suffix in ["α\n", "α\n", "β\n"] {
        let client = MockClient::new(Vec::new());
        let mut options = RunOptions::new("run-1", json!({"suffix": suffix}));
        options
            .adapter_skill_revisions
            .insert("claude-code".to_owned(), "agent-v3".to_owned());
        run_script(
            &source,
            Some(Path::new("resolved-prompt.js")),
            client.clone(),
            Rc::new(VecLifecycleSink::default()),
            options,
        )
        .unwrap();
        let submission = client.submissions.borrow()[0].clone();
        streams.push((
            submission.orchestration.prompt_revision,
            submission.orchestration.skill_revision,
            submission.payload_hash,
        ));
    }

    assert_eq!(streams[0], streams[1]);
    assert_eq!(
        streams[0].0.as_deref(),
        Some("sha256:100a1b066fe86cc024edd00424d7695640634d2fbf6d5ad195cad42cf9c59a72")
    );
    assert_eq!(streams[0].1.as_deref(), Some("agent-v3"));
    assert_ne!(streams[0].0, streams[2].0);
    assert_ne!(streams[0].2, streams[2].2);
}

#[test]
fn hardening_and_unhandled_rejections_fail_closed() {
    let banned = format!("{}\nMath.random();", meta(&["cpu"], &[]));
    let error = run(&banned, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowDeterminismError");
    assert_eq!(error.code, "determinism-violation");
    assert!(error.location.is_some());

    let dynamic_eval = format!(
        "{}\nconst compile = 'eval'; globalThis[compile]('40 + 2');",
        meta(&["cpu"], &[])
    );
    let error = run(&dynamic_eval, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowDeterminismError");
    assert_eq!(error.code, "determinism-violation");

    let deleted = format!(
        "{}\n({{date: 'Date' in globalThis, weak: 'WeakRef' in globalThis, finalization: 'FinalizationRegistry' in globalThis}});",
        meta(&["cpu"], &[])
    );
    let (report, _) = run(&deleted, MockClient::new(Vec::new())).unwrap();
    assert_eq!(
        report.final_value,
        Some(json!({"date": false, "weak": false, "finalization": false}))
    );

    let runaway = format!(
        "{}\ntry {{ while (true) {{}} }} catch (error) {{ 42; }}",
        meta(&["cpu"], &[])
    );
    let error = run(&runaway, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowRuntimeLimitError");
    assert_eq!(error.code, "runtime-limit");

    let rejected = format!(
        "{}\nPromise.reject(new Error('lost')); 42;",
        meta(&["cpu"], &[])
    );
    let error = run(&rejected, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowUnhandledRejection");
    assert_eq!(error.code, "unhandled-rejection");
    assert!(error.location.is_some());
}

#[test]
fn hardened_global_own_property_surface_is_pinned() {
    let source = format!(
        "{}\nObject.getOwnPropertyNames(globalThis).sort();",
        meta(&[], &[])
    );
    let (report, _) = run(&source, MockClient::new(Vec::new())).unwrap();
    assert_eq!(
        report.final_value,
        Some(json!([
            "AggregateError",
            "Array",
            "ArrayBuffer",
            "Atomics",
            "BigInt",
            "BigInt64Array",
            "BigUint64Array",
            "Boolean",
            "DataView",
            "Error",
            "EvalError",
            "Float16Array",
            "Float32Array",
            "Float64Array",
            "Function",
            "Infinity",
            "Int16Array",
            "Int32Array",
            "Int8Array",
            "JSON",
            "Map",
            "Math",
            "NaN",
            "Number",
            "Object",
            "Promise",
            "Proxy",
            "RangeError",
            "ReferenceError",
            "Reflect",
            "RegExp",
            "Set",
            "SharedArrayBuffer",
            "String",
            "Symbol",
            "SyntaxError",
            "TypeError",
            "TypedArray",
            "URIError",
            "Uint16Array",
            "Uint32Array",
            "Uint8Array",
            "Uint8ClampedArray",
            "WeakMap",
            "WeakSet",
            "__flowError",
            "__flowLocation",
            "args",
            "attributed",
            "claude",
            "codex",
            "decodeURI",
            "decodeURIComponent",
            "dissent",
            "drv",
            "encodeURI",
            "encodeURIComponent",
            "eval",
            "flowMeta",
            "globalThis",
            "isFinite",
            "isNaN",
            "job",
            "local",
            "log",
            "members",
            "parallel",
            "parseFloat",
            "parseInt",
            "pipeline",
            "quorum",
            "repairKey",
            "sh",
            "undefined"
        ]))
    );
}

#[test]
fn microtask_wall_clock_and_recursion_backstops_are_distinct() {
    let microtask_bomb = format!(
        "{}\nfunction spin() {{ Promise.resolve().then(spin); }} spin();",
        meta(&["cpu"], &[])
    );
    let mut options = RunOptions::new("run-1", json!({}));
    options.microtask_budget = 32;
    options.wall_clock_budget = Duration::from_secs(1);
    let error = run_script(
        &microtask_bomb,
        Some(Path::new("microtask-bomb.js")),
        MockClient::new(Vec::new()),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap_err();
    assert_eq!(error.name, "FlowRuntimeBudgetError");
    assert_eq!(error.code, "microtask-budget");

    let wall_clock = format!("{}\nPromise.resolve(42);", meta(&["cpu"], &[]));
    let mut options = RunOptions::new("run-1", json!({}));
    options.wall_clock_budget = Duration::ZERO;
    let error = run_script(
        &wall_clock,
        Some(Path::new("wall-clock.js")),
        MockClient::new(Vec::new()),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap_err();
    assert_eq!(error.name, "FlowRuntimeBudgetError");
    assert_eq!(error.code, "wall-clock-budget");

    let pending_host_work = format!(
        "{}\n(async () => sh(['hang'], {{pools: ['cpu']}}))()",
        meta(&["cpu"], &[])
    );
    let mut options = RunOptions::new("run-1", json!({}));
    options.wall_clock_budget = Duration::from_millis(30);
    let started = std::time::Instant::now();
    let error = run_script(
        &pending_host_work,
        Some(Path::new("pending-host-work.js")),
        MockClient::hanging(),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap_err();
    assert_eq!(error.name, "FlowRuntimeBudgetError");
    assert_eq!(error.code, "wall-clock-budget");
    assert!(started.elapsed() < Duration::from_millis(500));

    let recursion = format!(
        "{}\nfunction recurse() {{ return recurse(); }} recurse();",
        meta(&["cpu"], &[])
    );
    let error = run(&recursion, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowRuntimeLimitError");
    assert_eq!(error.code, "runtime-limit");
}

#[test]
fn wall_clock_budget_replay_reuses_prefix_and_completes_identically() {
    let source = format!(
        "{}\n(async () => {{\n\
         const steps = [];\n\
         log('before-first');\n\
         steps.push((await sh(['first'], {{pools: ['cpu']}})).result.step);\n\
         log('before-second');\n\
         steps.push((await sh(['second'], {{pools: ['cpu']}})).result.step);\n\
         log('before-third');\n\
         steps.push((await sh(['third'], {{pools: ['cpu']}})).result.step);\n\
         log('tail');\n\
         return steps;\n\
         }})()",
        meta(&["cpu"], &[])
    );
    let client = BudgetContinuationClient::new(Some(1));
    let first_sink = Rc::new(VecLifecycleSink::default());
    let mut first_options = RunOptions::new("run-1", json!({}));
    first_options.wall_clock_budget = Duration::from_millis(100);

    let error = run_script(
        &source,
        Some(Path::new("budget-continuation.js")),
        client.clone(),
        first_sink.clone(),
        first_options,
    )
    .unwrap_err();

    assert_eq!(error.name, "FlowRuntimeBudgetError");
    assert_eq!(error.code, "wall-clock-budget");
    assert_eq!(*client.executions.borrow(), [0, 1]);
    let first_events = first_sink.events();
    assert_eq!(
        first_events
            .iter()
            .filter(|event| event["type"] == "node-submitted")
            .map(|event| event["disposition"].clone())
            .collect::<Vec<_>>(),
        [json!("created"), json!("created")]
    );
    assert_eq!(
        first_events
            .iter()
            .filter(|event| event["type"] == "node-terminal")
            .map(|event| event["ordinal"].clone())
            .collect::<Vec<_>>(),
        [json!(0)]
    );

    let replay_sink = Rc::new(VecLifecycleSink::default());
    let replay = run_script(
        &source,
        Some(Path::new("budget-continuation.js")),
        client.clone(),
        replay_sink.clone(),
        RunOptions::new("run-1", json!({})),
    )
    .unwrap();

    let replay_events = replay_sink.events();
    assert_eq!(
        replay_events
            .iter()
            .filter(|event| event["type"] == "node-submitted")
            .map(|event| event["disposition"].clone())
            .collect::<Vec<_>>(),
        [json!("reused"), json!("attached"), json!("created")]
    );
    assert_eq!(
        replay_events
            .iter()
            .filter(|event| event["type"] == "node-terminal")
            .map(|event| event["disposition"].clone())
            .collect::<Vec<_>>(),
        [json!("reused"), json!("attached"), json!("created")]
    );
    assert_eq!(
        replay_events
            .iter()
            .filter(|event| event["type"] == "log")
            .map(|event| event["message"].clone())
            .collect::<Vec<_>>(),
        [json!("before-third"), json!("tail")]
    );
    assert_eq!(*client.executions.borrow(), [0, 1, 2]);
    assert_eq!(
        *client.admissions.borrow(),
        [
            (1, 0, Disposition::Created),
            (1, 1, Disposition::Created),
            (2, 0, Disposition::Reused),
            (2, 1, Disposition::Attached),
            (2, 2, Disposition::Created),
        ]
    );

    let uninterrupted_client = BudgetContinuationClient::new(None);
    let uninterrupted = run_script(
        &source,
        Some(Path::new("budget-continuation.js")),
        uninterrupted_client,
        Rc::new(VecLifecycleSink::default()),
        RunOptions::new("run-1", json!({})),
    )
    .unwrap();
    assert_eq!(replay.final_value, Some(json!([1, 2, 3])));
    assert_eq!(replay.final_value, uninterrupted.final_value);
}

#[test]
fn aggregate_and_loop_errors_keep_their_public_classes_and_call_sites() {
    let aggregate = format!(
        "{}\n(async () => parallel([\n\
         () => sh(['good'], {{pools: ['cpu']}}),\n\
         () => sh(['bad'], {{pools: ['cpu']}})\n\
         ]))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Created, 1),
        Reply {
            disposition: Disposition::Created,
            witness_seq: 2,
            verdict: Verdict::Failed,
            stderr_excerpt: None,
            stderr_truncated: None,
            result: None,
            divergent_hash: false,
            client_error: None,
        },
    ]);
    let error = run(&aggregate, client).unwrap_err();
    assert_eq!(error.name, "FlowAggregateError");
    assert_eq!(error.code, "aggregate-failure");
    assert!(error.location.is_some_and(|location| location.line > 1));

    let capped_meta = meta(&["cpu"], &[]).replace("selectors:", "iterationCap: 2,\nselectors:");
    let looped = format!(
        "{capped_meta}\n\
         function launch() {{ return sh(['work'], {{pools: ['cpu']}}); }}\n\
         launch();\n\
         launch();\n\
         launch();"
    );
    let error = run(&looped, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowLoopError");
    assert_eq!(error.code, "iteration-cap");
    assert!(error.location.is_some_and(|location| location.line > 1));
}

#[test]
fn documented_admission_terminal_and_node_cap_rejections_are_typed() {
    let source = format!(
        "{}\n(async () => sh(['work'], {{pools: ['cpu']}}))()",
        meta(&["cpu"], &[])
    );
    for (client_code, name) in [
        ("dedup-key-conflict", "FlowDedupKeyConflict"),
        ("admission-denied", "FlowAdmissionDenied"),
        ("flow-node-cap", "FlowNodeCapError"),
    ] {
        let error = run(
            &source,
            MockClient::new(vec![Reply::client_error(client_code)]),
        )
        .unwrap_err();
        assert_eq!(error.name, name);
        assert_eq!(error.code, client_code);
        assert!(error.location.is_some());
    }

    let failed = Reply {
        disposition: Disposition::Terminal,
        witness_seq: 1,
        verdict: Verdict::Failed,
        stderr_excerpt: Some("actionable child stderr\n".to_owned()),
        stderr_truncated: Some(false),
        result: None,
        divergent_hash: false,
        client_error: None,
    };
    let error = run(&source, MockClient::new(vec![failed])).unwrap_err();
    assert_eq!(error.name, "FlowTerminalError");
    assert_eq!(error.code, "terminal-failure");
    assert!(error.location.is_some());
    assert_eq!(
        error.details["node"]["stderrExcerpt"],
        "actionable child stderr\n"
    );
    assert_eq!(error.details["node"]["stderrTruncated"], false);

    let cancelled = Reply {
        disposition: Disposition::Terminal,
        witness_seq: 2,
        verdict: Verdict::Cancelled,
        stderr_excerpt: None,
        stderr_truncated: None,
        result: None,
        divergent_hash: false,
        client_error: None,
    };
    let error = run(&source, MockClient::new(vec![cancelled])).unwrap_err();
    assert_eq!(error.name, "FlowCancelledError");
    assert_eq!(error.code, "flow-cancelled");
    assert_eq!(error.details["node"]["verdict"], "cancelled");
}

#[test]
fn uncaught_exceptions_keep_stack_position_and_submission_frontier() {
    let source = format!(
        "{}\nfunction fail() {{ throw new Error('boom'); }}\nfail();",
        meta(&[], &[])
    );
    let error = run(&source, MockClient::new(Vec::new())).unwrap_err();
    let throw_line = source
        .lines()
        .position(|line| line.contains("throw new Error"))
        .unwrap() as u32
        + 1;
    assert_eq!(error.code, "script-evaluation");
    assert_eq!(error.location.unwrap().line, throw_line);
    assert_eq!(error.ordinal, Some(0));
    assert!(error
        .stack
        .as_deref()
        .is_some_and(|stack| stack.contains("test-flow.js")));

    let async_source = format!(
        "{}\nasync function fail() {{ throw new Error('async boom'); }}\nfail();",
        meta(&[], &[])
    );
    let async_error = run(&async_source, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(async_error.code, "script-evaluation");
    assert!(async_error
        .location
        .is_some_and(|location| location.line > 1));
    assert_eq!(async_error.ordinal, Some(0));
    assert!(async_error.stack.is_some());
}

#[test]
fn final_frontier_logs_flush_on_failure_and_unhandled_order_is_stable() {
    let source = format!(
        "{}\nlog('tail-before-failure');\nthrow new Error('boom');",
        meta(&[], &[])
    );
    let sink = Rc::new(VecLifecycleSink::default());
    let error = run_script(
        &source,
        Some(Path::new("tail-log.js")),
        MockClient::new(Vec::new()),
        sink.clone(),
        RunOptions::new("run-1", json!({})),
    )
    .unwrap_err();
    assert_eq!(error.code, "script-evaluation");
    assert_eq!(
        sink.events()
            .iter()
            .filter(|event| event["type"] == "log")
            .map(|event| event["message"].clone())
            .collect::<Vec<_>>(),
        [json!("tail-before-failure")]
    );

    let rejected = format!(
        "{}\nPromise.reject('first'); Promise.reject('second'); 42;",
        meta(&[], &[])
    );
    let error = run(&rejected, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "unhandled-rejection");
    assert_eq!(error.details["reason"], "first");
}

#[test]
fn canonical_payload_includes_resolved_nonoptional_defaults() {
    let source = format!(
        "{}\n(async () => sh(['true'], {{pools: ['cpu']}}))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(Vec::new());
    let mut options = RunOptions::new("run-1", json!({}));
    options.pool_credentials.insert(
        "cpu".to_owned(),
        BTreeMap::from([(
            "token".to_owned(),
            PathBuf::from("/run/credentials/cpu-token"),
        )]),
    );
    run_script(
        &source,
        Some(Path::new("test-flow.js")),
        client.clone(),
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .unwrap();
    let submissions = client.submissions.borrow();
    let submission = &submissions[0];
    assert_eq!(
        submission.spec.adapter_options,
        Some(json!({"prePromptArgv": [], "environment": {}}))
    );
    assert_eq!(
        submission.credentials,
        BTreeMap::from([(
            "token".to_owned(),
            PathBuf::from("/run/credentials/cpu-token")
        )])
    );
    let expected = json!({
        "argv": ["true"],
        "pool": "cpu",
        "adapter": "shell",
        "adapterOptions": {
            "prePromptArgv": [],
            "environment": {}
        },
        "evidence": [],
        "noEnqueue": true,
        "credentials": {
            "token": "/run/credentials/cpu-token"
        }
    });
    assert_eq!(
        submission.payload_hash,
        sha256(&serde_json::to_vec(&expected).unwrap())
    );
}

#[test]
fn job_and_agent_sugar_normalize_named_launch_policies_into_adapter_options() {
    let source = format!(
        "{}\n(async () => {{\n\
         await job({{\n\
           argv: ['direct'], adapter: 'codex', pools: ['codex-window'],\n\
           approvalPolicy: 'on-request', sandboxPolicy: 'workspace-write'\n\
         }});\n\
         return codex('implement', {{\n\
           approvalPolicy: 'on-request', sandboxPolicy: 'workspace-write'\n\
         }});\n\
         }})()",
        meta(&["codex-window"], &[])
    );
    let client = MockClient::new(Vec::new());
    run(&source, client.clone()).unwrap();

    let submissions = client.submissions.borrow();
    assert_eq!(submissions.len(), 2);
    for submission in submissions.iter() {
        assert_eq!(submission.spec.approval_policy, None);
        assert_eq!(submission.spec.sandbox_policy, None);
        assert_eq!(
            submission.spec.adapter_options,
            Some(json!({
                "prePromptArgv": [],
                "environment": {},
                "approvalPolicy": "on-request",
                "sandboxPolicy": "workspace-write"
            }))
        );
    }

    let private_envelope = format!(
        "{}\njob({{argv: ['x'], pools: ['codex-window'], adapterOptions: {{model: 'x'}}}});",
        meta(&["codex-window"], &[])
    );
    let error = run(&private_envelope, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "unknown-spec-field");
    assert_eq!(error.details["field"], "adapterOptions");
}

#[test]
fn a_brace_bodied_parallel_thunk_fails_the_run_instead_of_computing_on_undefined() {
    let source = format!(
        "{}\n(async () => parallel([\n\
         () => sh(['one'], {{pools: ['cpu']}}),\n\
         () => {{ sh(['two'], {{pools: ['cpu']}}); }}\n\
         ]))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Created, 1),
        Reply::pass(Disposition::Created, 2),
    ]);
    let error = run(&source, client).unwrap_err();
    assert_eq!(error.name, "FlowCombinatorError");
    assert_eq!(error.code, "parallel-invalid");
    assert!(
        error
            .message
            .starts_with("parallel() thunk 1 returned undefined instead of a promise;"),
        "unexpected message: {}",
        error.message
    );
    assert!(error.message.contains("remove the braces"));
    assert_eq!(error.details.get("index"), Some(&json!(1)));
    assert!(error.location.is_some_and(|location| location.line > 1));
}

#[test]
fn a_brace_bodied_parallel_thunk_fails_even_in_settle_mode() {
    let source = format!(
        "{}\n(async () => parallel([\n\
         () => {{ sh(['one'], {{pools: ['cpu']}}); }}\n\
         ], {{settle: true}}))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![Reply::pass(Disposition::Created, 1)]);
    let error = run(&source, client).unwrap_err();
    assert_eq!(error.code, "parallel-invalid");
}

#[test]
fn a_brace_bodied_pipeline_stage_fails_the_run_and_names_the_stage() {
    let source = format!(
        "{}\n(async () => pipeline(['a'],\n\
         (_previous, item) => sh([item, 'stage-1'], {{pools: ['cpu']}}),\n\
         (_previous, item) => {{ sh([item, 'stage-2'], {{pools: ['cpu']}}); }}\n\
         ))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![
        Reply::pass(Disposition::Created, 1),
        Reply::pass(Disposition::Created, 2),
    ]);
    let error = run(&source, client).unwrap_err();
    assert_eq!(error.name, "FlowCombinatorError");
    assert_eq!(error.code, "pipeline-invalid");
    assert!(
        error
            .message
            .starts_with("pipeline() stage 1 for item 0 returned undefined instead of a promise;"),
        "unexpected message: {}",
        error.message
    );
    assert!(error.message.contains("declare the stage async"));
    assert!(error.location.is_some_and(|location| location.line > 1));
}

#[test]
fn a_sugar_helper_rejects_a_fixed_field_at_evaluation_time_too() {
    let source = format!(
        "{}\nclaude('review', {{pools: ['claude-window']}});",
        meta(&["claude-window"], &[])
    );
    let error = run(&source, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowSpecError");
    assert_eq!(error.code, "sugar-option-conflict");
}

#[test]
fn unknown_field_messages_name_their_surface_and_list_what_is_accepted() {
    let job = format!(
        "{}\n(async () => job({{argv: ['x'], pools: ['cpu'], resultShema: {{}}}}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&job, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "unknown-spec-field");
    assert!(
        error
            .message
            .starts_with("unknown job spec field \"resultShema\", expected one of argv, adapter,"),
        "unexpected message: {}",
        error.message
    );
    assert!(error.message.contains("resultSchema"));

    let sugar = format!(
        "{}\n(async () => sh(['x'], {{pools: ['cpu'], resultShema: {{}}}}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&sugar, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "unknown-spec-field");
    assert!(
        error
            .message
            .starts_with("unknown sh() option \"resultShema\", expected one of argv, adapter,"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.details["expected"][0], "argv");
}

#[test]
fn duplicate_key_names_the_first_claim_by_ordinal_and_position() {
    let source = format!(
        "{}\n(async () => {{\n\
         for (const item of ['a', 'b']) {{\n\
         sh([item], {{pools: ['cpu'], key: 'constant'}});\n\
         }}\n\
         }})()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![Reply::pass(Disposition::Created, 1)]);
    let error = run(&source, client).unwrap_err();
    assert_eq!(error.code, "duplicate-key");
    assert!(
        error.message.starts_with(
            "flow-local key \"constant\" was already claimed by node 0 at line 11, column 1;"
        ),
        "unexpected message: {}",
        error.message
    );
    assert!(error.message.contains("derive the key from what varies"));
    assert_eq!(error.details["firstOrdinal"], 0);
    assert_eq!(error.details["firstLocation"]["line"], 11);
    // The second use is the same source line, which is exactly why the ordinal
    // is the part that tells the two apart.
    assert_eq!(error.location.unwrap().line, 11);
    assert_eq!(error.ordinal, Some(1));
}

#[test]
fn environment_name_errors_separate_invalid_from_reserved() {
    let invalid = format!(
        "{}\n(async () => sh(['x'], {{pools: ['cpu'], env: {{'9lives': 'x'}}}}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&invalid, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "reserved-env");
    assert!(
        error
            .message
            .starts_with("environment name \"9lives\" is not a valid name:"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.details["reason"], "invalid");

    let reserved = format!(
        "{}\n(async () => sh(['x'], {{pools: ['cpu'], env: {{TALLY_JOB_ID: 'x'}}}}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&reserved, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "reserved-env");
    assert!(
        error
            .message
            .starts_with("environment name \"TALLY_JOB_ID\" is reserved by the host:"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.details["reason"], "reserved");
}

#[test]
fn a_banned_global_says_what_to_do_instead() {
    let source = format!("{}\nconst now = Date.now();\nnow;", meta(&["cpu"], &[]));
    let error = run(&source, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "determinism-violation");
    assert_eq!(
        error.message,
        "banned global Date is unavailable in flow scripts because it would break replay; \
         witness a clock reading in a node instead"
    );

    let random = format!(
        "{}\n(async () => sh([String(Math.random())], {{pools: ['cpu']}}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&random, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.code, "determinism-violation");
    assert!(
        error
            .message
            .ends_with("derive the choice from witnessed input, or let members() pick, instead"),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn a_float_in_an_integer_field_says_so_instead_of_naming_a_rust_type() {
    let source = format!(
        "{}\n(async () => sh(['x'], {{pools: ['cpu'], runtimeMaxSec: Math.floor(600.5)}}))()",
        meta(&["cpu"], &[])
    );
    let error = run(&source, MockClient::new(Vec::new())).unwrap_err();
    assert_eq!(error.name, "FlowSpecError");
    assert_eq!(error.code, "invalid-spec");
    assert_eq!(
        error.message,
        "sh() options runtimeMaxSec must be a whole number, but arrived as the floating-point \
         value 600.0; JavaScript arithmetic such as Math.floor() stays floating point even when \
         its result is integral — coerce it with (x | 0)"
    );
    assert_eq!(error.details["field"], "runtimeMaxSec");

    // The coercion the message recommends actually works.
    let fixed = format!(
        "{}\n(async () => sh(['x'], {{pools: ['cpu'], runtimeMaxSec: Math.floor(600.5) | 0}}))()",
        meta(&["cpu"], &[])
    );
    let client = MockClient::new(vec![Reply::pass(Disposition::Created, 1)]);
    run(&fixed, client.clone()).unwrap();
    assert_eq!(
        client.submissions.borrow()[0].spec.runtime_max_sec,
        Some(600)
    );
}
