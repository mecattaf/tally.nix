use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::task::Poll;
use std::task::Waker;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use boa_engine::job::{GenericJob, Job, JobExecutor, NativeAsyncJob, PromiseJob};
use boa_engine::{Context, JsNativeError, JsResult, JsValue};

use crate::engine::{flow_to_js_error, HostShared};
use crate::FlowError;

struct DeadlineTimer {
    expired: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
    cancel: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl DeadlineTimer {
    fn new(remaining: Duration) -> Self {
        let expired = Arc::new(AtomicBool::new(remaining.is_zero()));
        let waker = Arc::new(Mutex::new(None::<Waker>));
        if remaining.is_zero() {
            return Self {
                expired,
                waker,
                cancel: None,
                thread: None,
            };
        }
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let thread_expired = expired.clone();
        let thread_waker = waker.clone();
        let thread = std::thread::spawn(move || {
            if matches!(
                cancel_rx.recv_timeout(remaining),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                thread_expired.store(true, Ordering::Release);
                if let Some(waker) = thread_waker.lock().ok().and_then(|mut slot| slot.take()) {
                    waker.wake();
                }
            }
        });
        Self {
            expired,
            waker,
            cancel: Some(cancel_tx),
            thread: Some(thread),
        }
    }

    fn is_expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    fn register(&self, waker: &Waker) {
        if self.is_expired() {
            waker.wake_by_ref();
            return;
        }
        if let Ok(mut slot) = self.waker.lock() {
            *slot = Some(waker.clone());
        }
        if self.is_expired() {
            if let Ok(mut slot) = self.waker.lock() {
                slot.take();
            }
            waker.wake_by_ref();
        }
    }
}

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Boa executor which owns promise ordering for a flow run.
pub(crate) struct FlowJobExecutor {
    promise_jobs: RefCell<VecDeque<PromiseJob>>,
    async_jobs: RefCell<VecDeque<NativeAsyncJob>>,
    generic_jobs: RefCell<VecDeque<GenericJob>>,
    rejected_timeout: Cell<bool>,
    started_at: Instant,
    wall_clock_budget: Duration,
    microtask_budget: u64,
    microtasks_executed: Cell<u64>,
    shared: Rc<HostShared>,
}

impl FlowJobExecutor {
    pub(crate) fn new(
        shared: Rc<HostShared>,
        wall_clock_budget: Duration,
        microtask_budget: u64,
    ) -> Self {
        Self {
            promise_jobs: RefCell::default(),
            async_jobs: RefCell::default(),
            generic_jobs: RefCell::default(),
            rejected_timeout: Cell::new(false),
            started_at: Instant::now(),
            wall_clock_budget,
            microtask_budget,
            microtasks_executed: Cell::new(0),
            shared,
        }
    }

    fn clear(&self) {
        self.promise_jobs.borrow_mut().clear();
        self.async_jobs.borrow_mut().clear();
        self.generic_jobs.borrow_mut().clear();
    }

    fn budget_error(
        &self,
        context: &RefCell<&mut Context>,
        code: &'static str,
        message: String,
    ) -> boa_engine::JsError {
        self.clear();
        let error = FlowError::new("FlowRuntimeBudgetError", code, message);
        flow_to_js_error(error, &mut context.borrow_mut())
    }

    fn check_wall_clock(&self, context: &RefCell<&mut Context>) -> JsResult<()> {
        if self.started_at.elapsed() < self.wall_clock_budget {
            return Ok(());
        }
        Err(self.budget_error(
            context,
            "wall-clock-budget",
            format!(
                "flow JavaScript exceeded its {}ms wall-clock budget",
                self.wall_clock_budget.as_millis()
            ),
        ))
    }

    fn charge_microtask(&self, context: &RefCell<&mut Context>) -> JsResult<()> {
        let Some(next) = self.microtasks_executed.get().checked_add(1) else {
            return Err(self.budget_error(
                context,
                "microtask-budget",
                "flow JavaScript microtask counter overflowed".to_owned(),
            ));
        };
        if next > self.microtask_budget {
            return Err(self.budget_error(
                context,
                "microtask-budget",
                format!(
                    "flow JavaScript exceeded its {}-microtask budget",
                    self.microtask_budget
                ),
            ));
        }
        self.microtasks_executed.set(next);
        Ok(())
    }

    fn drain_synchronous(&self, context: &RefCell<&mut Context>) -> JsResult<bool> {
        let mut progressed = false;
        loop {
            self.check_wall_clock(context)?;
            let promise_jobs = std::mem::take(&mut *self.promise_jobs.borrow_mut());
            let generic_jobs = std::mem::take(&mut *self.generic_jobs.borrow_mut());
            if promise_jobs.is_empty() && generic_jobs.is_empty() {
                break;
            }
            progressed = true;
            for job in promise_jobs {
                self.charge_microtask(context)?;
                job.call(&mut context.borrow_mut())?;
                self.check_wall_clock(context)?;
            }
            for job in generic_jobs {
                self.charge_microtask(context)?;
                job.call(&mut context.borrow_mut())?;
                self.check_wall_clock(context)?;
            }
        }
        context.borrow_mut().clear_kept_objects();
        Ok(progressed)
    }

    async fn drive(self: Rc<Self>, context: &RefCell<&mut Context>) -> JsResult<()> {
        type ActiveFuture<'a> = Pin<Box<dyn Future<Output = JsResult<JsValue>> + 'a>>;

        let remaining = self
            .wall_clock_budget
            .saturating_sub(self.started_at.elapsed());
        let deadline_timer = DeadlineTimer::new(remaining);
        let mut active: Vec<Option<ActiveFuture<'_>>> = Vec::new();
        loop {
            self.check_wall_clock(context)?;
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
            let deadline_expired = poll_fn(|cx| {
                if deadline_timer.is_expired() {
                    return Poll::Ready(true);
                }
                deadline_timer.register(cx.waker());
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
                    Poll::Ready(false)
                } else {
                    Poll::Pending
                }
            })
            .await;
            if deadline_expired {
                self.check_wall_clock(context)?;
            }

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
