use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

use boa_engine::job::{GenericJob, Job, JobExecutor, NativeAsyncJob, PromiseJob};
use boa_engine::{Context, JsNativeError, JsResult, JsValue};

use crate::engine::HostShared;

/// Boa executor which owns promise ordering for a flow run.
pub(crate) struct FlowJobExecutor {
    promise_jobs: RefCell<VecDeque<PromiseJob>>,
    async_jobs: RefCell<VecDeque<NativeAsyncJob>>,
    generic_jobs: RefCell<VecDeque<GenericJob>>,
    rejected_timeout: Cell<bool>,
    shared: Rc<HostShared>,
}

impl FlowJobExecutor {
    pub(crate) fn new(shared: Rc<HostShared>) -> Self {
        Self {
            promise_jobs: RefCell::default(),
            async_jobs: RefCell::default(),
            generic_jobs: RefCell::default(),
            rejected_timeout: Cell::new(false),
            shared,
        }
    }

    fn clear(&self) {
        self.promise_jobs.borrow_mut().clear();
        self.async_jobs.borrow_mut().clear();
        self.generic_jobs.borrow_mut().clear();
    }

    fn drain_synchronous(&self, context: &RefCell<&mut Context>) -> JsResult<bool> {
        let mut progressed = false;
        loop {
            let promise_jobs = std::mem::take(&mut *self.promise_jobs.borrow_mut());
            let generic_jobs = std::mem::take(&mut *self.generic_jobs.borrow_mut());
            if promise_jobs.is_empty() && generic_jobs.is_empty() {
                break;
            }
            progressed = true;
            for job in promise_jobs {
                job.call(&mut context.borrow_mut())?;
            }
            for job in generic_jobs {
                job.call(&mut context.borrow_mut())?;
            }
        }
        context.borrow_mut().clear_kept_objects();
        Ok(progressed)
    }

    async fn drive(self: Rc<Self>, context: &RefCell<&mut Context>) -> JsResult<()> {
        type ActiveFuture<'a> = Pin<Box<dyn Future<Output = JsResult<JsValue>> + 'a>>;

        let mut active: Vec<Option<ActiveFuture<'_>>> = Vec::new();
        loop {
            if self.rejected_timeout.get() {
                self.clear();
                return Err(JsNativeError::error()
                    .with_message(
                        "FlowDeterminismError [determinism-violation]: timer jobs are forbidden",
                    )
                    .into());
            }
            if self.shared.fatal_error().is_some() {
                self.clear();
                return Err(JsNativeError::error()
                    .with_message("flow run aborted after a fatal replay error")
                    .into());
            }

            self.drain_synchronous(context)?;
            for job in std::mem::take(&mut *self.async_jobs.borrow_mut()) {
                active.push(Some(Box::pin(job.call(context))));
            }

            if active.is_empty()
                && self.promise_jobs.borrow().is_empty()
                && self.async_jobs.borrow().is_empty()
                && self.generic_jobs.borrow().is_empty()
            {
                break;
            }

            let mut completed = Vec::new();
            poll_fn(|cx| {
                let mut progressed = false;
                for (index, future) in active.iter_mut().enumerate() {
                    let Some(future) = future.as_mut() else {
                        continue;
                    };
                    if let Poll::Ready(result) = future.as_mut().poll(cx) {
                        completed.push((index, result));
                        progressed = true;
                    }
                }
                if progressed || self.shared.has_ready_observation() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;

            for (index, result) in completed.drain(..) {
                active[index] = None;
                if let Err(error) = result {
                    self.clear();
                    return Err(error);
                }
            }
            active.retain(Option::is_some);

            // Let continuations from the previously released node materialize their next
            // jobs before choosing another ready observation. This is what gives pipeline()
            // per-item progress without a hidden stage barrier.
            self.drain_synchronous(context)?;
            if self.async_jobs.borrow().is_empty() {
                // A host future registers a terminal result and yields. Releasing exactly one
                // minimum-witnessSeq waiter per turn keeps promise jobs FIFO in ledger order.
                self.shared.release_ready_observation();
            }
            futures_lite::future::yield_now().await;
        }
        Ok(())
    }
}

impl JobExecutor for FlowJobExecutor {
    fn enqueue_job(self: Rc<Self>, job: Job, _context: &mut Context) {
        match job {
            Job::PromiseJob(job) => self.promise_jobs.borrow_mut().push_back(job),
            Job::AsyncJob(job) => self.async_jobs.borrow_mut().push_back(job),
            Job::GenericJob(job) => self.generic_jobs.borrow_mut().push_back(job),
            Job::TimeoutJob(_) => self.rejected_timeout.set(true),
            _ => self.rejected_timeout.set(true),
        }
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        futures_lite::future::block_on(self.drive(&RefCell::new(context)))
    }

    async fn run_jobs_async(self: Rc<Self>, context: &RefCell<&mut Context>) -> JsResult<()>
    where
        Self: Sized,
    {
        self.drive(context).await
    }
}
