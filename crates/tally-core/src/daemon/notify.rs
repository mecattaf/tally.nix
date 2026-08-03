use super::*;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, PoisonError};

/// What the daemon must prove before a `WATCHDOG=1` may be sent on its behalf.
///
/// The keepalive runs on its own OS thread precisely so that a busy daemon
/// cannot delay the datagram — but that also means the thread has no first-hand
/// knowledge of whether the daemon is still working. It therefore never speaks
/// for itself: it forwards a ping only while this witness is fresh.
///
/// The witness is stamped at the top of the dispatch loop, before every
/// `select!`, and it is the *only* thing the keepalive consults. That is
/// deliberate. A witness stamped by a task on the runtime would be the tighter
/// of the two and would silently govern every case: the runtime is
/// single-threaded, so a blocking `sync_all` or `flock` — which is what the
/// expensive part of a terminal witness append or a lifecycle compaction
/// actually is — stops such a task exactly as it stops the loop, and the
/// daemon would get the tight bound precisely where it needs the loose one. One
/// witness means a stall costs the same whether the thread is parked on an
/// `await` or blocked in a syscall.
///
/// The 100 ms lease tick is what makes the witness meaningful: a healthy loop
/// re-enters at least at 10 Hz even with nothing else to do, so staleness means
/// one arm's body has stopped returning rather than that the daemon is idle.
#[derive(Debug)]
pub(crate) struct DispatchProgress {
    origin: Instant,
    stamped_millis: AtomicU64,
}

impl DispatchProgress {
    fn new() -> Self {
        let progress = Self {
            origin: Instant::now(),
            stamped_millis: AtomicU64::new(0),
        };
        progress.stamp();
        progress
    }

    pub(crate) fn stamp(&self) {
        self.stamped_millis
            .store(self.elapsed_millis(), Ordering::Relaxed);
    }

    fn age(&self) -> Duration {
        Duration::from_millis(
            self.elapsed_millis()
                .saturating_sub(self.stamped_millis.load(Ordering::Relaxed)),
        )
    }

    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// How often the keepalive thread wakes to consider sending a ping.
pub(crate) fn keepalive_cadence(watchdog: Duration) -> Duration {
    watchdog
        .checked_div(4)
        .unwrap_or(Duration::from_micros(1))
        .max(Duration::from_micros(1))
}

/// When an overdue dispatch loop stops being silent. A loop that has not come
/// back around for two service periods is already abnormal, and the operator
/// must not have to wait for [`dispatch_stall_horizon`] to hear about it.
pub(crate) fn dispatch_stall_notice(watchdog: Duration) -> Duration {
    watchdog.saturating_mul(2)
}

/// The headroom the dispatch loop gets before the keepalive stops standing for
/// it. A `select!` arm body that is merely slow must not cost the daemon its
/// life; one that never returns still must, so the budget is finite.
pub(crate) fn dispatch_stall_horizon(watchdog: Duration) -> Duration {
    watchdog.saturating_mul(10)
}

/// What the keepalive thread owes systemd for a dispatch loop of a given age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeepaliveVerdict {
    Ping,
    /// Overdue, but still inside the headroom: ping, and say so.
    PingOverdue,
    /// Past the headroom: the service watchdog must be allowed to act.
    Withhold,
}

pub(crate) fn keepalive_verdict(
    age: Duration,
    notice: Duration,
    horizon: Duration,
) -> KeepaliveVerdict {
    if age > horizon {
        KeepaliveVerdict::Withhold
    } else if age > notice {
        KeepaliveVerdict::PingOverdue
    } else {
        KeepaliveVerdict::Ping
    }
}

/// The systemd watchdog keepalive, running on a thread of its own.
///
/// It owns no daemon state and takes no daemon locks, so nothing the daemon
/// does can delay the datagram. What it can do is decline to send one.
pub(crate) struct WatchdogKeepalive {
    progress: Arc<DispatchProgress>,
    stop: Arc<(std::sync::Mutex<bool>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WatchdogKeepalive {
    pub(crate) fn progress(&self) -> Arc<DispatchProgress> {
        Arc::clone(&self.progress)
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
        let progress = Arc::new(DispatchProgress::new());
        let stop = Arc::new((std::sync::Mutex::new(false), Condvar::new()));
        let cadence = keepalive_cadence(watchdog);
        let notice = dispatch_stall_notice(watchdog);
        let horizon = dispatch_stall_horizon(watchdog);
        let notifier = self.clone();
        let thread_progress = Arc::clone(&progress);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("tally-watchdog".to_owned())
            .spawn(move || {
                let notice_millis = notice.as_millis().max(1);
                let mut announced = 0;
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
                    let age = thread_progress.age();
                    match keepalive_verdict(age, notice, horizon) {
                        KeepaliveVerdict::Withhold => {
                            if !withheld {
                                withheld = true;
                                eprintln!(
                                    "tally: the daemon dispatch loop has not re-entered its \
                                     select for {} ms, which is past its {} ms headroom; \
                                     withholding the systemd watchdog keepalive so the service \
                                     watchdog can act",
                                    age.as_millis(),
                                    horizon.as_millis()
                                );
                            }
                            continue;
                        }
                        // Still pinging, but an overdue loop is reported while
                        // it is overdue rather than only once it is fatal.
                        KeepaliveVerdict::PingOverdue => {
                            let elapsed_notices = age.as_millis() / notice_millis;
                            if elapsed_notices > announced {
                                announced = elapsed_notices;
                                eprintln!(
                                    "tally: the daemon dispatch loop has not re-entered its \
                                     select for {} ms; still keeping the service watchdog alive \
                                     for up to {} ms",
                                    age.as_millis(),
                                    horizon.as_millis()
                                );
                            }
                        }
                        KeepaliveVerdict::Ping => {
                            if withheld || announced > 0 {
                                eprintln!(
                                    "tally: the daemon dispatch loop is running again after {} ms",
                                    age.as_millis()
                                );
                            }
                            withheld = false;
                            announced = 0;
                        }
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
                // A daemon that cannot start its keepalive is killed by the
                // service watchdog within one period. Say why now, on the
                // surface an operator reads, rather than leaving the restart
                // unexplained.
                eprintln!(
                    "tally: the systemd watchdog keepalive thread could not be started: {error}"
                );
                return None;
            }
        };
        Some(WatchdogKeepalive {
            progress,
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
