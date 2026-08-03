use super::*;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, PoisonError};

/// What the daemon must prove before a `WATCHDOG=1` may be sent on its behalf.
///
/// The keepalive runs on its own OS thread precisely so that a busy daemon
/// cannot delay the datagram — but that also means the thread has no first-hand
/// knowledge of whether the daemon is still working. It therefore never speaks
/// for itself: it forwards a ping only while both witnesses below are fresh.
/// When either goes stale the keepalive falls silent and systemd's own timer
/// runs to completion, so a wedged daemon is still killed loudly rather than
/// held alive by a keepalive that has stopped meaning anything.
#[derive(Debug)]
pub(crate) struct DaemonLiveness {
    origin: Instant,
    /// Stamped by a task on the daemon's runtime. Proves the executor is still
    /// scheduling work — it goes stale when the runtime thread is blocked in
    /// synchronous code or deadlocked.
    scheduler_millis: AtomicU64,
    /// Stamped at the top of the dispatch loop, before every `select!`. Proves
    /// the loop still comes back around. The 100 ms lease tick is what makes
    /// this witness meaningful: a healthy loop re-enters at least at 10 Hz even
    /// with nothing else to do, so staleness here means one arm's body has
    /// stopped returning rather than that the daemon happens to be idle.
    dispatch_millis: AtomicU64,
}

impl DaemonLiveness {
    fn new() -> Self {
        let liveness = Self {
            origin: Instant::now(),
            scheduler_millis: AtomicU64::new(0),
            dispatch_millis: AtomicU64::new(0),
        };
        liveness.stamp_scheduler();
        liveness.stamp_dispatch();
        liveness
    }

    pub(crate) fn stamp_scheduler(&self) {
        self.scheduler_millis
            .store(self.elapsed_millis(), Ordering::Relaxed);
    }

    pub(crate) fn stamp_dispatch(&self) {
        self.dispatch_millis
            .store(self.elapsed_millis(), Ordering::Relaxed);
    }

    fn scheduler_age(&self) -> Duration {
        self.age(&self.scheduler_millis)
    }

    fn dispatch_age(&self) -> Duration {
        self.age(&self.dispatch_millis)
    }

    fn age(&self, stamp: &AtomicU64) -> Duration {
        Duration::from_millis(
            self.elapsed_millis()
                .saturating_sub(stamp.load(Ordering::Relaxed)),
        )
    }

    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// How often the keepalive thread wakes to consider sending a ping.
fn keepalive_cadence(watchdog: Duration) -> Duration {
    fraction(watchdog, 4)
}

/// How often the daemon's runtime stamps the scheduler witness.
fn scheduler_cadence(watchdog: Duration) -> Duration {
    fraction(watchdog, 8)
}

/// The oldest scheduler evidence a ping may stand on. Half the service period
/// keeps the claim conservative: systemd never learns of a liveness observation
/// more than half a watchdog period after it was made.
fn scheduler_horizon(watchdog: Duration) -> Duration {
    fraction(watchdog, 2)
}

/// The headroom the dispatch loop gets. A `select!` arm whose body is merely
/// slow — a witness fsync, a lifecycle compaction, a terminal transaction under
/// an estate-sized context — must not cost the daemon its life, which is the
/// whole defect being repaired. A body that never returns still must, so the
/// budget is finite: ten service periods, after which the keepalive stops and
/// systemd restarts the daemon exactly as it does today.
fn dispatch_horizon(watchdog: Duration) -> Duration {
    watchdog.saturating_mul(10)
}

fn fraction(watchdog: Duration, divisor: u32) -> Duration {
    watchdog
        .checked_div(divisor)
        .unwrap_or(Duration::from_micros(1))
        .max(Duration::from_micros(1))
}

/// The systemd watchdog keepalive, running on a thread of its own.
///
/// It owns no daemon state and takes no daemon locks, so nothing the daemon
/// does can delay the datagram. What it can do is decline to send one.
pub(crate) struct WatchdogKeepalive {
    liveness: Arc<DaemonLiveness>,
    scheduler_cadence: Duration,
    stop: Arc<(std::sync::Mutex<bool>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WatchdogKeepalive {
    pub(crate) fn liveness(&self) -> Arc<DaemonLiveness> {
        Arc::clone(&self.liveness)
    }

    pub(crate) const fn scheduler_cadence(&self) -> Duration {
        self.scheduler_cadence
    }

    /// Stop pinging and join the thread. Called before `STOPPING=1` so that no
    /// keepalive can follow the daemon's own announcement that it is going away.
    pub(crate) fn shutdown(&mut self) {
        {
            let (lock, condvar) = &*self.stop;
            let mut stopped = lock.lock().unwrap_or_else(PoisonError::into_inner);
            *stopped = true;
            condvar.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for WatchdogKeepalive {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug, Clone)]
pub struct SystemdNotifier {
    socket: Option<PathBuf>,
    watchdog: Option<Duration>,
}

impl SystemdNotifier {
    pub fn from_environment() -> Result<Self, DaemonError> {
        let socket = std::env::var_os("NOTIFY_SOCKET").map(PathBuf::from);
        let watchdog = match std::env::var("WATCHDOG_USEC") {
            Ok(value) => {
                if let Ok(pid) = std::env::var("WATCHDOG_PID") {
                    let pid = pid
                        .parse::<u32>()
                        .map_err(|_| DaemonError::Notify("WATCHDOG_PID is invalid".to_owned()))?;
                    if pid != std::process::id() {
                        None
                    } else {
                        Some(parse_watchdog(&value)?)
                    }
                } else {
                    Some(parse_watchdog(&value)?)
                }
            }
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(DaemonError::Notify(error.to_string())),
        };
        Ok(Self { socket, watchdog })
    }

    pub fn with_socket(socket: PathBuf, watchdog: Option<Duration>) -> Self {
        Self {
            socket: Some(socket),
            watchdog,
        }
    }

    fn send(&self, payload: &str) -> Result<(), DaemonError> {
        let Some(socket) = &self.socket else {
            return Ok(());
        };
        if socket.as_os_str().as_encoded_bytes().starts_with(b"@") {
            return send_abstract_notify(socket, payload.as_bytes());
        }
        let datagram =
            UnixDatagram::unbound().map_err(|error| DaemonError::Notify(error.to_string()))?;
        datagram
            .send_to(payload.as_bytes(), socket)
            .map_err(|error| DaemonError::Notify(error.to_string()))?;
        Ok(())
    }

    pub fn ready(&self) -> Result<(), DaemonError> {
        self.send("READY=1\nSTATUS=tally daemon ready")
    }

    pub fn watchdog(&self) -> Result<(), DaemonError> {
        self.send("WATCHDOG=1")
    }

    pub fn stopping(&self) -> Result<(), DaemonError> {
        self.send("STOPPING=1")
    }

    /// Start the keepalive thread, if this service is watched at all.
    ///
    /// `fatal` carries a send failure back into the daemon's own fatal channel:
    /// the previous in-loop keepalive ended the run loop when the notify socket
    /// refused a datagram, and that is still the outcome.
    pub(crate) fn keepalive(
        &self,
        fatal: mpsc::UnboundedSender<DaemonError>,
    ) -> Option<WatchdogKeepalive> {
        let watchdog = self.watchdog?;
        let liveness = Arc::new(DaemonLiveness::new());
        let stop = Arc::new((std::sync::Mutex::new(false), Condvar::new()));
        let cadence = keepalive_cadence(watchdog);
        let scheduler_horizon = scheduler_horizon(watchdog);
        let dispatch_horizon = dispatch_horizon(watchdog);
        let notifier = self.clone();
        let thread_liveness = Arc::clone(&liveness);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("tally-watchdog".to_owned())
            .spawn(move || {
                let mut withheld = false;
                loop {
                    {
                        let (lock, condvar) = &*thread_stop;
                        let stopped = lock.lock().unwrap_or_else(PoisonError::into_inner);
                        if *stopped {
                            break;
                        }
                        let (stopped, _) = condvar
                            .wait_timeout(stopped, cadence)
                            .unwrap_or_else(PoisonError::into_inner);
                        if *stopped {
                            break;
                        }
                    }
                    let scheduler_age = thread_liveness.scheduler_age();
                    let dispatch_age = thread_liveness.dispatch_age();
                    let stale = if scheduler_age > scheduler_horizon {
                        Some(format!(
                            "the daemon runtime has not run a task for {} ms",
                            scheduler_age.as_millis()
                        ))
                    } else if dispatch_age > dispatch_horizon {
                        Some(format!(
                            "the daemon dispatch loop has not re-entered its select for {} ms",
                            dispatch_age.as_millis()
                        ))
                    } else {
                        None
                    };
                    if let Some(reason) = stale {
                        if !withheld {
                            withheld = true;
                            eprintln!(
                                "tally: {reason}; withholding the systemd watchdog keepalive so \
                                 the service watchdog can act"
                            );
                        }
                        continue;
                    }
                    if withheld {
                        withheld = false;
                        eprintln!(
                            "tally: the daemon is making progress again; the systemd watchdog \
                             keepalive has resumed"
                        );
                    }
                    if let Err(error) = notifier.watchdog() {
                        let _ = fatal.send(error);
                        break;
                    }
                }
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                // A daemon that cannot start its keepalive would be killed by
                // the service watchdog within one period. Say why now, on the
                // surface an operator reads, rather than leaving the restart
                // unexplained.
                eprintln!(
                    "tally: the systemd watchdog keepalive thread could not be started: {error}"
                );
                return None;
            }
        };
        Some(WatchdogKeepalive {
            liveness,
            scheduler_cadence: scheduler_cadence(watchdog),
            stop,
            thread: Some(thread),
        })
    }
}

fn parse_watchdog(value: &str) -> Result<Duration, DaemonError> {
    let micros = value
        .parse::<u64>()
        .map_err(|_| DaemonError::Notify("WATCHDOG_USEC is invalid".to_owned()))?;
    if micros == 0 {
        return Err(DaemonError::Notify(
            "WATCHDOG_USEC must be positive".to_owned(),
        ));
    }
    Ok(Duration::from_micros(micros))
}

fn send_abstract_notify(socket: &Path, payload: &[u8]) -> Result<(), DaemonError> {
    use std::mem::size_of;
    use std::os::fd::RawFd;
    use std::os::unix::ffi::OsStrExt;

    let bytes = socket.as_os_str().as_bytes();
    let name = bytes
        .strip_prefix(b"@")
        .ok_or_else(|| DaemonError::Notify("abstract notify path is invalid".to_owned()))?;
    if name.is_empty() || name.len() >= 108 {
        return Err(DaemonError::Notify(
            "abstract notify path length is invalid".to_owned(),
        ));
    }
    let fd: RawFd =
        unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(DaemonError::Notify(io::Error::last_os_error().to_string()));
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (index, byte) in name.iter().enumerate() {
        address.sun_path[index + 1] = *byte as libc::c_char;
    }
    let length = (size_of::<libc::sa_family_t>() + 1 + name.len()) as libc::socklen_t;
    let sent = unsafe {
        libc::sendto(
            fd,
            payload.as_ptr().cast(),
            payload.len(),
            0,
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    };
    let error = (sent < 0).then(io::Error::last_os_error);
    unsafe {
        libc::close(fd);
    }
    if let Some(error) = error {
        return Err(DaemonError::Notify(error.to_string()));
    }
    Ok(())
}
