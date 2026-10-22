//! Temporary, attempt-scoped LAN QR-login challenge. No account authorization
//! is granted by the local connection; normal polling/manual authentication wins.
use crate::{protobuf as p, wire, DeviceProfile};
use anyhow::{ensure, Result};
use std::{
    fmt,
    io::{self, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub(crate) const CGI: u16 = 22312;
pub(crate) const PATH: &str = "/cgi-bin/micromsg-bin/extdeviceuploadlaninfo";
const MAX_TOKEN: usize = 4096;
const TICK: Duration = Duration::from_millis(20);

#[derive(Debug)]
struct LanError(&'static str);
impl fmt::Display for LanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LAN login challenge failed ({})", self.0)
    }
}
impl std::error::Error for LanError {}
pub(crate) fn failure(reason: &'static str) -> anyhow::Error {
    LanError(reason).into()
}
pub fn login_lan_error_reason(error: &anyhow::Error) -> Option<&'static str> {
    error.downcast_ref::<LanError>().map(|e| e.0)
}

#[derive(Clone)]
pub(crate) struct Scope {
    deadline: Instant,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl Scope {
    pub(crate) fn new(
        deadline: Instant,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            deadline,
            cancelled: Arc::new(cancelled),
        }
    }
    fn active(&self) -> bool {
        Instant::now() < self.deadline && !(self.cancelled)()
    }
}
struct State {
    stop: AtomicBool,
    sent: AtomicBool,
    done: AtomicBool,
    scope: Scope,
}
impl State {
    fn active(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && self.scope.active()
    }
}

pub(crate) struct Challenge {
    addresses: Vec<Ipv4Addr>,
    port: u16,
    state: Arc<State>,
    token: Mutex<Option<mpsc::SyncSender<Zeroizing<Vec<u8>>>>>,
    worker: Option<JoinHandle<()>>,
}
impl Challenge {
    pub(crate) fn bind(scope: Scope) -> Result<Self> {
        if !scope.active() {
            return Err(failure("cancelled_or_expired"));
        }
        let addresses = wechat_platform::local_ipv4_addresses()
            .map_err(|_| failure("interface_enumeration_failed"))?;
        if addresses.is_empty() {
            return Err(failure("no_ipv4_address"));
        }
        Self::from_listener(bind_ports(8050..=8100)?, addresses, scope)
    }
    pub(crate) fn from_listener(
        listener: TcpListener,
        addresses: Vec<Ipv4Addr>,
        scope: Scope,
    ) -> Result<Self> {
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(State {
            stop: AtomicBool::new(false),
            sent: AtomicBool::new(false),
            done: AtomicBool::new(false),
            scope,
        });
        let (token, incoming) = mpsc::sync_channel::<Zeroizing<Vec<u8>>>(1);
        let shared = state.clone();
        let worker = thread::Builder::new()
            .name("wechat-lan-login".into())
            .spawn(move || {
                // Keep early peer connections queued until the authenticated upload
                // supplies a token. Never reply with an empty/uninitialized token.
                let frame = loop {
                    if !shared.active() {
                        shared.done.store(true, Ordering::Release);
                        return;
                    }
                    match incoming.recv_timeout(TICK) {
                        Ok(frame) => break frame,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => {
                            shared.done.store(true, Ordering::Release);
                            return;
                        }
                    }
                };
                while shared.active() {
                    match listener.accept() {
                        Ok((mut stream, _peer)) => {
                            if write_frame(&mut stream, &frame, &shared).is_ok() {
                                shared.sent.store(true, Ordering::Release);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                    thread::sleep(TICK);
                }
                shared.done.store(true, Ordering::Release);
            })?;
        Ok(Self {
            addresses,
            port,
            state,
            token: Mutex::new(Some(token)),
            worker: Some(worker),
        })
    }
    pub(crate) fn request(
        &self,
        device: &DeviceProfile,
        identifier: &[u8],
        random: &[u8; 16],
    ) -> Result<Vec<u8>> {
        request(device, identifier, &self.addresses, self.port, random)
    }
    pub(crate) fn arm(&self, token: &[u8]) -> Result<()> {
        if !self.active() {
            return Err(failure("cancelled_or_expired"));
        }
        let frame = reply_frame(token).map_err(|_| failure("invalid_token"))?;
        self.token
            .lock()
            .map_err(|_| failure("listener_unavailable"))?
            .take()
            .ok_or_else(|| failure("already_armed"))?
            .send(frame)
            .map_err(|_| failure("listener_unavailable"))
    }
    pub(crate) fn active(&self) -> bool {
        self.state.active() && !self.state.done.load(Ordering::Acquire)
    }
    pub(crate) fn status(&self) -> &'static str {
        if !self.active() {
            "stopped"
        } else if self.state.sent.load(Ordering::Acquire) {
            "token_sent"
        } else {
            "waiting_for_phone"
        }
    }
    pub(crate) fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
    }
}
impl Drop for Challenge {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn bind_ports(ports: impl IntoIterator<Item = u16>) -> Result<TcpListener> {
    for port in ports {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
            Err(_) => return Err(failure("listener_unavailable")),
        }
    }
    Err(failure("ports_unavailable"))
}
fn write_frame(stream: &mut TcpStream, frame: &[u8], state: &State) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let deadline = (Instant::now() + Duration::from_secs(1)).min(state.scope.deadline);
    let mut offset = 0;
    while offset < frame.len() {
        if !state.active() || Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "LAN reply ended"));
        }
        match stream.write(&frame[offset..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => offset += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(TICK),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) fn request(
    device: &DeviceProfile,
    identifier: &[u8],
    addresses: &[Ipv4Addr],
    port: u16,
    random: &[u8; 16],
) -> Result<Vec<u8>> {
    ensure!(
        !identifier.is_empty() && identifier.len() <= 4096,
        "Invalid login identifier"
    );
    ensure!(
        !addresses.is_empty() && addresses.len() <= 64 && port != 0,
        "Invalid LAN address list"
    );
    let mut entries = Vec::new();
    for ip in addresses {
        let entry = p::message(&[
            p::bytes(1, ip.to_string().as_bytes()),
            p::uint(2, port as u64),
        ])?;
        entries.push(p::bytes(1, &entry));
    }
    p::message(&[
        p::bytes(1, &wire::base_request(device, 0, 0)?),
        p::bytes(2, &p::message(&entries)?),
        p::bytes(3, identifier),
        p::bytes(4, &p::buffer(random)),
    ])
}
pub(crate) fn response(input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(input.len() <= 16384, "LAN upload response too large");
    let fields = p::decode(input)?;
    ensure!(p::base_code(&fields)? == 0, "LAN upload rejected");
    let token = p::data(&fields, 2)?;
    ensure!(
        !token.is_empty() && token.len() <= MAX_TOKEN,
        "Invalid LAN token length"
    );
    std::str::from_utf8(token)?;
    Ok(Zeroizing::new(token.to_vec()))
}
fn reply_frame(token: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        !token.is_empty() && token.len() <= MAX_TOKEN,
        "Invalid LAN token length"
    );
    let token = std::str::from_utf8(token)?;
    #[derive(serde::Serialize)]
    struct Reply<'a> {
        scene: &'static str,
        token: &'a str,
    }
    let json = Zeroizing::new(serde_json::to_vec(&Reply {
        scene: "xwechat",
        token,
    })?);
    // mars::lan_cs default packer: big-endian u32 total size, including itself.
    let mut frame = Zeroizing::new(Vec::with_capacity(json.len() + 4));
    frame.extend_from_slice(&u32::try_from(json.len() + 4)?.to_be_bytes());
    frame.extend_from_slice(&json);
    Ok(frame)
}

#[cfg(test)]
#[path = "lan_login_tests.rs"]
mod tests;
