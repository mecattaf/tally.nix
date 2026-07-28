use super::*;

pub type SupervisedFuture = Pin<Box<dyn Future<Output = Result<(), String>>>>;
pub type SupervisedFactory = Rc<dyn Fn() -> SupervisedFuture>;

#[derive(Clone)]
pub struct SupervisedTask {
    pub name: String,
    pub restart_delay: Duration,
    pub factory: SupervisedFactory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionEvent {
    Started {
        name: String,
        generation: u64,
    },
    Restarting {
        name: String,
        generation: u64,
        reason: String,
    },
}

pub fn spawn_supervised(
    task: SupervisedTask,
    mut shutdown: watch::Receiver<bool>,
    events: mpsc::UnboundedSender<SupervisionEvent>,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        let mut generation = 0_u64;
        loop {
            if *shutdown.borrow() {
                break;
            }
            generation = generation.saturating_add(1);
            let _ = events.send(SupervisionEvent::Started {
                name: task.name.clone(),
                generation,
            });
            let mut child = tokio::task::spawn_local((task.factory)());
            let reason = tokio::select! {
                result = &mut child => match result {
                    Ok(Ok(())) => "producer exited".to_owned(),
                    Ok(Err(error)) => error,
                    Err(error) if error.is_panic() => "producer panicked".to_owned(),
                    Err(error) => format!("producer join failed: {error}"),
                },
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        child.abort();
                        let _ = child.await;
                        break;
                    }
                    continue;
                }
            };
            let _ = events.send(SupervisionEvent::Restarting {
                name: task.name.clone(),
                generation,
                reason,
            });
            tokio::select! {
                _ = tokio::time::sleep(task.restart_delay) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    })
}
