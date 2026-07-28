use super::*;

pub(crate) async fn watchdog_tick(interval: &mut Option<tokio::time::Interval>) {
    if let Some(interval) = interval {
        interval.tick().await;
    } else {
        std::future::pending::<()>().await;
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

    pub(crate) fn watchdog_interval(&self) -> Option<tokio::time::Interval> {
        self.watchdog.map(|duration| {
            let cadence = duration.checked_div(2).unwrap_or(Duration::from_micros(1));
            let mut interval = tokio::time::interval(cadence.max(Duration::from_micros(1)));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval
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
