use super::*;

#[derive(Debug, Clone)]
pub(super) struct CapturedTrace {
    pub(super) location: SourceLocation,
    pub(super) stack: String,
}

pub(super) struct FlowHooks {
    rejected: RefCell<Vec<JsObject>>,
    root_promises: RefCell<HashSet<JsObject>>,
    rejection_traces: RefCell<HashMap<JsObject, CapturedTrace>>,
}

impl FlowHooks {
    pub(super) fn new() -> Self {
        Self {
            rejected: RefCell::default(),
            root_promises: RefCell::default(),
            rejection_traces: RefCell::default(),
        }
    }

    pub(super) fn observe_root(&self, promise: JsObject) {
        self.root_promises.borrow_mut().insert(promise.clone());
        self.rejected
            .borrow_mut()
            .retain(|rejected| rejected != &promise);
    }

    pub(super) fn unhandled(&self) -> Vec<JsObject> {
        self.rejected.borrow().clone()
    }

    pub(super) fn rejection_trace(&self, promise: &JsObject) -> Option<CapturedTrace> {
        self.rejection_traces.borrow().get(promise).cloned()
    }
}

impl HostHooks for FlowHooks {
    fn promise_rejection_tracker(
        &self,
        promise: &JsObject,
        operation: OperationType,
        context: &mut Context,
    ) {
        match operation {
            OperationType::Reject => {
                if !self.root_promises.borrow().contains(promise) {
                    let mut rejected = self.rejected.borrow_mut();
                    if !rejected.iter().any(|candidate| candidate == promise) {
                        rejected.push(promise.clone());
                    }
                }
                if let Some(trace) = capture_trace(context) {
                    self.rejection_traces
                        .borrow_mut()
                        .insert(promise.clone(), trace);
                }
            }
            OperationType::Handle => {
                self.rejected
                    .borrow_mut()
                    .retain(|rejected| rejected != promise);
            }
        }
    }

    fn ensure_can_compile_strings(
        &self,
        _realm: Realm,
        _parameters: &[JsString],
        _body: &JsString,
        _direct: bool,
        context: &mut Context,
    ) -> JsResult<()> {
        Err(flow_to_js_error(
            FlowError::determinism(
                "eval",
                "runtime string compilation through eval or Function is forbidden",
                call_site(context),
            ),
            context,
        ))
    }
}
