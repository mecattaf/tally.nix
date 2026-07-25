use std::future::Future;
use std::pin::Pin;

use crate::{Admission, ClientError, FlowSubmission, NodeResult, RunInspection};

pub type FlowFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Daemon boundary owned by the runner.
///
/// Deterministic mocks and the live single-connection transport implement the same
/// disposition, terminal-result, and run-inspection contract.
pub trait FlowClient {
    fn inspect_run<'a>(
        &'a self,
        flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>>;

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>>;

    fn await_terminal<'a>(
        &'a self,
        task_uuid: &'a str,
        attempt: u32,
    ) -> FlowFuture<'a, Result<NodeResult, ClientError>>;
}
