use super::*;

#[derive(Debug, Clone)]
pub(crate) enum CommitCommand {
    Upsert {
        row: Box<RowSeed>,
        status: Status,
        labor_class: LaborClass,
    },
    Rebuild,
    Shutdown,
}

pub(crate) trait ReplicaCommitter: Send {
    fn commit<'a>(
        &'a mut self,
        command: CommitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;
}

pub(crate) struct TaskDbCommitter {
    pub(crate) db: TaskDb,
    pub(crate) events_dir: PathBuf,
    pub(crate) witness_path: PathBuf,
    pub(crate) adapter_metadata: BTreeMap<Uuid, (RowSeed, Status, LaborClass)>,
}

impl ReplicaCommitter for TaskDbCommitter {
    fn commit<'a>(
        &'a mut self,
        command: CommitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
        Box::pin(async move {
            match command {
                CommitCommand::Upsert {
                    row,
                    status,
                    labor_class,
                } => {
                    let row = *row;
                    if row.session_ref.is_some()
                        || row.model.is_some()
                        || row.final_message.is_some()
                    {
                        self.adapter_metadata
                            .insert(row.uuid, (row.clone(), status.clone(), labor_class));
                    } else {
                        self.adapter_metadata.remove(&row.uuid);
                    }
                    let prepared = self
                        .db
                        .prepare_row(row, status, labor_class)
                        .await
                        .map_err(|error| error.to_string())?;
                    self.db
                        .commit_prepared([prepared])
                        .await
                        .map_err(|error| error.to_string())?;
                }
                CommitCommand::Rebuild => {
                    self.db
                        .rebuild_from_sources(&self.events_dir, &self.witness_path)
                        .await
                        .map_err(|error| error.to_string())?;
                    let metadata = self.adapter_metadata.values().cloned().collect::<Vec<_>>();
                    let mut prepared = Vec::with_capacity(metadata.len());
                    for (row, status, labor_class) in metadata {
                        prepared.push(
                            self.db
                                .prepare_row(row, status, labor_class)
                                .await
                                .map_err(|error| error.to_string())?,
                        );
                    }
                    self.db
                        .commit_prepared(prepared)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                CommitCommand::Shutdown => {}
            }
            Ok(())
        })
    }
}

pub(crate) struct CommitWorker {
    pub(crate) thread: std::thread::JoinHandle<()>,
    pub(crate) stopping: Arc<AtomicBool>,
}

pub(crate) fn spawn_commit_worker(
    mut committer: Box<dyn ReplicaCommitter>,
    mut receiver: mpsc::UnboundedReceiver<CommitCommand>,
    state_lock: File,
) -> Result<CommitWorker, DaemonError> {
    let stopping = Arc::new(AtomicBool::new(false));
    let worker_stopping = stopping.clone();
    let thread = std::thread::Builder::new()
        .name("tally-replica-commit".to_owned())
        .spawn(move || {
            // A wedged post-ack writer may outlive the bounded daemon shutdown.
            // Retain the daemon lock in that thread so no replacement writer can
            // open the same state until the old worker has actually stopped.
            let _state_lock = state_lock;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the replica worker runtime must initialize");
            while let Some(command) = receiver.blocking_recv() {
                if worker_stopping.load(Ordering::Acquire)
                    || matches!(command, CommitCommand::Shutdown)
                {
                    break;
                }
                if let Err(error) = runtime.block_on(committer.commit(command)) {
                    eprintln!("tally: post-ack replica commit failed: {error}");
                }
                if worker_stopping.load(Ordering::Acquire) {
                    break;
                }
            }
        })
        .map_err(|error| DaemonError::Invalid(format!("cannot start replica worker: {error}")))?;
    Ok(CommitWorker { thread, stopping })
}
