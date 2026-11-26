//! Pluggable async stream, HTTP, and clock abstractions for native and
//! WebAssembly (`wasm32-unknown-unknown`) hosts.
//!
//! Host allowlists, route validation, receive budgets, and MMTLS/business
//! authentication remain enforced inside `wechat-protocol` before handing
//! requests to a transport.
use anyhow::{bail, ensure, Result};
use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};
use url::Url;

/// `Send` bound on native targets, unconstrained on single-threaded `wasm32`.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}

/// Unconstrained marker on single-threaded `wasm32` (`Send` on native targets).
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// `Sync` bound on native targets, unconstrained on single-threaded `wasm32`.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> MaybeSync for T {}

/// Unconstrained marker on single-threaded `wasm32` (`Sync` on native targets).
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSync for T {}

/// Boxed transport future: `Send` on native targets and `?Send` on `wasm32`.
#[cfg(not(target_arch = "wasm32"))]
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed transport future: `Send` on native targets and `?Send` on `wasm32`.
#[cfg(target_arch = "wasm32")]
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Typed transport failure categories shared across native and custom
/// transports for retryable login-poll, call-RPC, CDN failover, and media
/// receive-budget classification.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// Connection, read, or write deadline elapsed.
    Timeout,
    /// Connection attempt failed or the peer closed the connection unexpectedly.
    Disconnected,
    /// The connection was established but failed or closed before the
    /// response completed. CDN transfers retry the same address once for
    /// this, as for a dropped keep-alive connection, rather than failing over.
    Interrupted,
    /// The HTTP response body exceeded `HttpRequest::max_response_bytes`.
    ResponseTooLarge,
    /// The destination is, or resolves to, an address the transport's policy
    /// refuses, such as `DestinationPolicy::PublicOnly` on native targets. No
    /// connection was made.
    DestinationNotAllowed,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("Transport operation timed out"),
            Self::Disconnected => f.write_str("Transport disconnected"),
            Self::Interrupted => f.write_str("Transport connection interrupted"),
            Self::ResponseTooLarge => f.write_str("HTTP response body exceeds receive budget"),
            Self::DestinationNotAllowed => f.write_str("Transport destination not allowed"),
        }
    }
}

impl std::error::Error for TransportError {}

pub(crate) fn find_transport_error(error: &anyhow::Error) -> Option<TransportError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<TransportError>().copied())
}

/// Monotonic and wall-clock time source for deadlines, short-link ticket
/// lifetimes, and wire timestamps.
///
/// Native hosts can use [`SystemClock`]. On `wasm32-unknown-unknown`, calling
/// `std::time::Instant::now()` or `std::time::SystemTime::now()` panics at
/// runtime, so callers supply an implementation backed by host time.
pub trait Clock: MaybeSend + MaybeSync {
    /// Monotonic elapsed milliseconds from an arbitrary fixed epoch.
    /// Return values must never decrease.
    fn monotonic_ms(&self) -> u64;

    /// Current Unix timestamp in milliseconds since `1970-01-01T00:00:00Z`.
    fn unix_time_ms(&self) -> Result<u64>;

    /// Current Unix timestamp in whole seconds as a 32-bit wire timestamp.
    fn unix_time_secs(&self) -> Result<u32> {
        u32::try_from(self.unix_time_ms()? / 1000)
            .map_err(|_| anyhow::anyhow!("Clock timestamp out of u32 range"))
    }

    /// Asynchronously wait for `duration`.
    ///
    /// Hosts should override this when retry backoff is desired (for example
    /// `setTimeout` on `wasm32` or a runtime timer on native). The default
    /// implementation resolves immediately so minimal or test clocks do not
    /// require a timer.
    fn sleep<'a>(&'a self, _duration: Duration) -> TransportFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }
}

impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn monotonic_ms(&self) -> u64 {
        (**self).monotonic_ms()
    }

    fn unix_time_ms(&self) -> Result<u64> {
        (**self).unix_time_ms()
    }

    fn unix_time_secs(&self) -> Result<u32> {
        (**self).unix_time_secs()
    }

    fn sleep<'a>(&'a self, duration: Duration) -> TransportFuture<'a, ()> {
        (**self).sleep(duration)
    }
}

/// System-backed [`Clock`] using `std::time::Instant` and `std::time::SystemTime`.
///
/// [`Clock::sleep`] uses `tokio::time::sleep` when called inside a Tokio
/// runtime (requiring Tokio's time driver to be enabled on that runtime) and
/// falls back to `std::thread::sleep` outside Tokio.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct SystemClock {
    origin: std::time::Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl SystemClock {
    /// Construct a clock anchored at the current monotonic instant.
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Clock for SystemClock {
    fn monotonic_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn unix_time_ms(&self) -> Result<u64> {
        let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
        u64::try_from(elapsed.as_millis())
            .map_err(|_| anyhow::anyhow!("System clock timestamp overflow"))
    }

    fn sleep<'a>(&'a self, duration: Duration) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if duration.is_zero() {
                return;
            }
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::time::sleep(duration).await;
            } else {
                std::thread::sleep(duration);
            }
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn native_monotonic_ms() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(std::time::Instant::now);
    u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Result of a bounded [`AsyncByteStream::read`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// `1..=buf.len()` bytes were written into the caller's buffer.
    Bytes(usize),
    /// The peer closed the stream cleanly before sending more bytes.
    Eof,
    /// The per-call timeout elapsed without new bytes arriving.
    TimedOut,
}

/// Bidirectional ordered byte stream for MMTLS long-link sessions and
/// plaintext HTTP/1.1-over-TCP exchanges.
pub trait AsyncByteStream: MaybeSend {
    /// Read at least one byte (up to `buf.len()`) before `timeout` elapses.
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
        timeout: Duration,
    ) -> TransportFuture<'a, Result<ReadOutcome>>;

    /// Write every byte in `buf` before `timeout` elapses.
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
        timeout: Duration,
    ) -> TransportFuture<'a, Result<()>>;

    /// Best-effort prompt teardown of the underlying stream.
    ///
    /// Implementations must return promptly and must not block indefinitely
    /// waiting for peer shutdown acknowledgement; callers such as
    /// [`HttpOverStream`] and [`crate::mmtls::AsyncSession`] await `close`
    /// without an additional timeout.
    fn close<'a>(&'a mut self) -> TransportFuture<'a, Result<()>>;
}

/// Factory for raw ordered byte streams (such as TCP or Direct Sockets).
///
/// Host allowlists are validated inside `wechat-protocol` before `connect` is
/// called.
pub trait StreamTransport: MaybeSend + MaybeSync {
    /// Open a raw byte stream to `host:port` within `timeout`.
    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        timeout: Duration,
    ) -> TransportFuture<'a, Result<Box<dyn AsyncByteStream>>>;
}

impl<T: StreamTransport + ?Sized> StreamTransport for Arc<T> {
    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        timeout: Duration,
    ) -> TransportFuture<'a, Result<Box<dyn AsyncByteStream>>> {
        (**self).connect(host, port, timeout)
    }
}

/// HTTP request method supported by protocol and media exchanges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    /// HTTP `GET`.
    Get,
    /// HTTP `POST`.
    Post,
}

impl HttpMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// Bounded HTTP request prepared by `wechat-protocol` after host, route, and
/// payload validation.
///
/// Implementations must not follow HTTP redirects automatically.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    /// HTTP method (`GET` or `POST`).
    pub method: HttpMethod,
    /// Validated target URL.
    pub url: Url,
    /// Additional request headers `(name, value)`.
    pub headers: Vec<(String, String)>,
    /// Request body bytes (empty for `GET`).
    pub body: Vec<u8>,
    /// End-to-end request deadline.
    pub timeout: Duration,
    /// Maximum response body bytes permitted before failing closed.
    pub max_response_bytes: usize,
}

/// Bounded HTTP response returned by an [`HttpTransport`].
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// Numeric HTTP status code (`100..=599`).
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Response body bytes (at most `request.max_response_bytes`).
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Return the first header value matching `name` (ASCII case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Return all header values matching `name` (ASCII case-insensitive), in
    /// wire order.
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Parse and validate `Content-Length` if present. Rejects malformed values
    /// and conflicting duplicate headers.
    pub(crate) fn content_length(&self) -> Result<Option<u64>> {
        let mut parsed = None;
        for raw in self.header_values("content-length") {
            let trimmed = raw.trim();
            ensure!(
                !trimmed.is_empty()
                    && trimmed.len() <= 20
                    && trimmed.bytes().all(|b| b.is_ascii_digit()),
                "Invalid HTTP Content-Length header"
            );
            let value = trimmed
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("Invalid HTTP Content-Length header"))?;
            if let Some(prev) = parsed {
                ensure!(prev == value, "Conflicting HTTP Content-Length headers");
            } else {
                parsed = Some(value);
            }
        }
        Ok(parsed)
    }
}

/// Status and headers of an HTTP response whose body is delivered separately
/// to an [`HttpBodySink`].
#[derive(Clone, Debug)]
pub struct HttpResponseHead {
    /// Numeric HTTP status code (`100..=599`).
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
}

impl HttpResponseHead {
    /// Return the first header value matching `name` (ASCII case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Return all header values matching `name` (ASCII case-insensitive), in
    /// wire order.
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Parse and validate `Content-Length` if present, rejecting malformed
    /// values and conflicting duplicates.
    pub(crate) fn content_length(&self) -> Result<Option<u64>> {
        HttpResponse {
            status: self.status,
            headers: self.headers.clone(),
            body: Vec::new(),
        }
        .content_length()
    }
}

/// Receives an HTTP response streamed by [`HttpTransport::send_streaming`].
pub trait HttpBodySink: MaybeSend {
    /// Inspect the status and headers before any body bytes arrive. An error
    /// stops the transfer and is returned to the caller unchanged.
    fn head(&mut self, head: &HttpResponseHead) -> Result<()>;

    /// Accept the next piece of the body, in order. Never called with an
    /// empty slice. An error stops the transfer and is returned to the caller
    /// unchanged.
    fn chunk(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Asynchronous HTTP request/response transport.
///
/// Implementations must disable automatic redirects and honor
/// `request.timeout` and `request.max_response_bytes`. Returning
/// [`TransportError::Disconnected`] or [`TransportError::Timeout`] classifies
/// transient failures for CDN IP failover and retry backoff; returning
/// [`TransportError::ResponseTooLarge`] lets media downloaders preserve their
/// exact size-limit error classification when a response exceeds
/// `request.max_response_bytes`. Callers with only raw TCP sockets can wrap a
/// [`StreamTransport`] in [`HttpOverStream`] for plaintext WeChat endpoints
/// (`http://{host}/mmtls/...` and `http://{ip}:443/...`).
pub trait HttpTransport: MaybeSend + MaybeSync {
    /// Execute a single bounded HTTP request without automatic redirects.
    fn send<'a>(&'a self, request: HttpRequest) -> TransportFuture<'a, Result<HttpResponse>>;

    /// Execute a request like [`Self::send`], delivering the head and then
    /// the body in pieces to `sink` as they arrive, so a caller can report
    /// progress or stop a long download early. The whole body is still
    /// bounded by `request.max_response_bytes` and `request.timeout`.
    ///
    /// The default implementation buffers the response with [`Self::send`]
    /// and delivers the body in one piece; transports that can stream should
    /// override it.
    fn send_streaming<'a>(
        &'a self,
        request: HttpRequest,
        sink: &'a mut dyn HttpBodySink,
    ) -> TransportFuture<'a, Result<HttpResponseHead>> {
        Box::pin(async move {
            let response = self.send(request).await?;
            let head = HttpResponseHead {
                status: response.status,
                headers: response.headers,
            };
            sink.head(&head)?;
            if !response.body.is_empty() {
                sink.chunk(&response.body)?;
            }
            Ok(head)
        })
    }
}

impl<T: HttpTransport + ?Sized> HttpTransport for Arc<T> {
    fn send<'a>(&'a self, request: HttpRequest) -> TransportFuture<'a, Result<HttpResponse>> {
        (**self).send(request)
    }

    fn send_streaming<'a>(
        &'a self,
        request: HttpRequest,
        sink: &'a mut dyn HttpBodySink,
    ) -> TransportFuture<'a, Result<HttpResponseHead>> {
        (**self).send_streaming(request, sink)
    }
}

/// Background task spawner used by [`ProtocolTransport`] so submitted account
/// exchanges continue holding the account handoff barrier and adopt verified
/// cookies and renewed credentials even if the caller drops the outer future.
///
/// On native targets running inside a Tokio runtime, [`ProtocolTransport`]
/// detaches exchanges onto the current Tokio runtime automatically when no
/// custom spawner is configured. On `wasm32` (for example via
/// `wasm_bindgen_futures::spawn_local`) or non-Tokio executors, configure a
/// spawner with [`ProtocolTransport::with_spawner`] whenever callers may cancel
/// or time out account RPC futures.
pub trait TaskSpawner: MaybeSend + MaybeSync {
    /// Spawn `task` to run to completion independently of the caller's future.
    fn spawn(&self, task: TransportFuture<'static, ()>);
}

impl<T: TaskSpawner + ?Sized> TaskSpawner for Arc<T> {
    fn spawn(&self, task: TransportFuture<'static, ()>) {
        (**self).spawn(task)
    }
}

/// Pluggable async transport bundle for [`crate::NativeClient`] and async login
/// operations.
///
/// When a [`TaskSpawner`] is attached via [`Self::with_spawner`] (or when
/// executing inside a native Tokio runtime), account-owned RPCs, renewals, and
/// logout submissions detach from the caller's future so dropping the future
/// after submission still holds the account barrier and adopts verified replies.
/// On non-Tokio executors without a configured [`TaskSpawner`] (including
/// `wasm32` without `with_spawner`), exchanges run inline and callers must not
/// cancel account RPC futures after submission.
#[derive(Clone)]
pub struct ProtocolTransport {
    /// Raw byte-stream transport for MMTLS long-link handshakes and persistent
    /// push connections.
    pub streams: Arc<dyn StreamTransport>,
    /// HTTP transport for MMTLS short-link exchanges.
    pub http: Arc<dyn HttpTransport>,
    /// Monotonic and wall-clock time source.
    pub clock: Arc<dyn Clock>,
    spawner: Option<Arc<dyn TaskSpawner>>,
    /// Whether async login may open a local TCP listener for a LAN risk
    /// challenge.
    #[cfg(not(target_arch = "wasm32"))]
    lan_login: bool,
}

impl ProtocolTransport {
    /// Construct a transport bundle with explicit stream, HTTP, and clock
    /// implementations.
    pub fn new(
        streams: Arc<dyn StreamTransport>,
        http: Arc<dyn HttpTransport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            streams,
            http,
            clock,
            spawner: None,
            #[cfg(not(target_arch = "wasm32"))]
            lan_login: false,
        }
    }

    /// Construct a transport bundle backed solely by a [`StreamTransport`] and
    /// [`Clock`], routing short-link HTTP exchanges through [`HttpOverStream`].
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub fn from_streams(streams: Arc<dyn StreamTransport>, clock: Arc<dyn Clock>) -> Self {
        let http: Arc<dyn HttpTransport> =
            Arc::new(HttpOverStream::new(streams.clone(), clock.clone()));
        Self {
            streams,
            http,
            clock,
            spawner: None,
            #[cfg(not(target_arch = "wasm32"))]
            lan_login: false,
        }
    }

    /// Attach a [`TaskSpawner`] so submitted account exchanges detach from the
    /// caller's future, preserving handoff barriers and reply adoption across
    /// caller cancellation on non-Tokio and `wasm32` executors.
    pub fn with_spawner(mut self, spawner: Arc<dyn TaskSpawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Return the configured [`TaskSpawner`], if any.
    pub fn spawner(&self) -> Option<&Arc<dyn TaskSpawner>> {
        self.spawner.as_ref()
    }

    /// Let async QR login answer a LAN risk challenge by opening a temporary
    /// local TCP listener (ports 8050–8100 on this host's IPv4 addresses), as
    /// the blocking login does. [`Self::native`] enables this; enable it on
    /// another bundle only when the host may accept connections from the
    /// phone on the local network.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_lan_login(mut self) -> Self {
        self.lan_login = true;
        self
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn lan_login(&self) -> bool {
        self.lan_login
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[path = "native_transport.rs"]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{DestinationPolicy, NativeHttpTransport, TokioSpawner, TokioStreamTransport};

const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP_HEADERS: usize = 128;
const MAX_CHUNK_LINE_BYTES: usize = 64;

/// Strict plaintext HTTP/1.1 client layered on top of a [`StreamTransport`].
///
/// Only `http://` URLs are accepted; `https://` URLs are refused rather than
/// downgraded. Redirects are never followed. Header counts/sizes, response
/// bodies, and deadlines are strictly bounded.
pub struct HttpOverStream<S> {
    streams: S,
    clock: Arc<dyn Clock>,
}

impl<S> HttpOverStream<S> {
    /// Wrap `streams` with a strict plaintext HTTP/1.1 client using `clock`
    /// for end-to-end request deadlines.
    pub fn new(streams: S, clock: Arc<dyn Clock>) -> Self {
        Self { streams, clock }
    }
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn valid_header_value(value: &str) -> bool {
    value.len() <= 8192
        && value
            .bytes()
            .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

fn format_http_request(request: &HttpRequest) -> Result<(String, u16, Vec<u8>)> {
    ensure!(
        request.url.scheme() == "http",
        "HttpOverStream only supports plaintext http:// URLs"
    );
    ensure!(
        request.url.username().is_empty() && request.url.password().is_none(),
        "HTTP request URL must not contain credentials"
    );
    ensure!(
        !request.timeout.is_zero() && request.timeout <= Duration::from_secs(1800),
        "Invalid HTTP request timeout"
    );
    let raw_host = request
        .url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Missing HTTP request host"))?;
    let connect_host = raw_host.trim_matches(['[', ']']).to_string();
    ensure!(
        !connect_host.is_empty() && !raw_host.bytes().any(|b| b <= 0x20 || b >= 0x7f),
        "Invalid HTTP request host"
    );
    let port = request.url.port_or_known_default().unwrap_or(80);
    ensure!(port != 0, "Invalid HTTP request port");
    let host_header = match request.url.port() {
        Some(p) if p != 80 => format!("{raw_host}:{p}"),
        _ => raw_host.to_string(),
    };
    let mut target = request.url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = request.url.query() {
        target.push('?');
        target.push_str(query);
    }
    ensure!(
        target.starts_with('/') && !target.bytes().any(|b| b <= 0x20 || b >= 0x7f),
        "Invalid HTTP request target"
    );
    let mut wire = Vec::with_capacity(256 + request.body.len());
    wire.extend_from_slice(request.method.as_str().as_bytes());
    wire.push(b' ');
    wire.extend_from_slice(target.as_bytes());
    wire.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    wire.extend_from_slice(host_header.as_bytes());
    wire.extend_from_slice(b"\r\n");
    if request.method == HttpMethod::Post || !request.body.is_empty() {
        wire.extend_from_slice(format!("Content-Length: {}\r\n", request.body.len()).as_bytes());
    }
    for (name, value) in &request.headers {
        check_request_header(name, value)?;
        if name.eq_ignore_ascii_case("connection") {
            continue;
        }
        wire.extend_from_slice(name.as_bytes());
        wire.extend_from_slice(b": ");
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"Connection: close\r\n\r\n");
    wire.extend_from_slice(&request.body);
    Ok((connect_host, port, wire))
}

/// Rules every transport applies to a caller-supplied request header: a valid
/// name and value, no `Host`, `Content-Length` or `Transfer-Encoding` (the
/// transport derives those), and `Connection` only as `close`.
pub(crate) fn check_request_header(name: &str, value: &str) -> Result<()> {
    ensure!(valid_header_name(name), "Invalid HTTP header name");
    ensure!(valid_header_value(value), "Invalid HTTP header value");
    if name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("transfer-encoding")
    {
        bail!("Restricted HTTP request header");
    }
    if name.eq_ignore_ascii_case("connection") {
        ensure!(
            value.eq_ignore_ascii_case("close"),
            "Only Connection: close is supported"
        );
    }
    Ok(())
}

fn remaining_deadline(clock: &dyn Clock, deadline_ms: u64) -> Result<Duration> {
    let now = clock.monotonic_ms();
    ensure!(now < deadline_ms, TransportError::Timeout);
    Ok(Duration::from_millis(deadline_ms - now))
}

async fn read_chunk_into(
    stream: &mut dyn AsyncByteStream,
    buf: &mut [u8],
    clock: &dyn Clock,
    deadline_ms: u64,
) -> Result<Option<usize>> {
    let remaining = remaining_deadline(clock, deadline_ms)?;
    match stream.read(buf, remaining).await? {
        ReadOutcome::Bytes(0) => bail!("Empty transport stream read"),
        ReadOutcome::Bytes(n) => {
            ensure!(n <= buf.len(), "Transport stream read exceeded buffer");
            Ok(Some(n))
        }
        ReadOutcome::Eof => Ok(None),
        ReadOutcome::TimedOut => Err(TransportError::Timeout.into()),
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[derive(Debug)]
struct ParsedHead {
    status: u16,
    headers: Vec<(String, String)>,
    chunked: bool,
    content_length: Option<usize>,
}

fn parse_response_head(raw: &[u8]) -> Result<ParsedHead> {
    let text =
        std::str::from_utf8(raw).map_err(|_| anyhow::anyhow!("Invalid HTTP response UTF-8"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing HTTP status line"))?;
    let rest = status_line
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| status_line.strip_prefix("HTTP/1.0 "))
        .ok_or_else(|| anyhow::anyhow!("Unsupported HTTP response version"))?;
    ensure!(
        rest.len() >= 3
            && rest.as_bytes()[..3].iter().all(u8::is_ascii_digit)
            && (rest.len() == 3 || rest.as_bytes()[3] == b' '),
        "Invalid HTTP status code"
    );
    ensure!(
        !status_line.contains(['\r', '\n']),
        "Malformed HTTP status line"
    );
    let status: u16 = rest[..3].parse()?;
    ensure!(
        (200..=599).contains(&status),
        "Unsupported HTTP response status {status}"
    );
    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        ensure!(
            headers.len() < MAX_HTTP_HEADERS,
            "Too many HTTP response headers"
        );
        ensure!(
            !line.contains(['\r', '\n']),
            "Malformed HTTP response header"
        );
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("Malformed HTTP response header"))?;
        let value = value.trim();
        ensure!(valid_header_name(name), "Invalid HTTP response header name");
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(
                !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "Invalid HTTP Content-Length"
            );
            let parsed = value
                .parse::<usize>()
                .map_err(|_| anyhow::anyhow!("HTTP Content-Length overflow"))?;
            if let Some(previous) = content_length {
                ensure!(
                    previous == parsed,
                    "Conflicting HTTP Content-Length headers"
                );
            }
            content_length = Some(parsed);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            ensure!(!chunked, "Duplicate HTTP Transfer-Encoding header");
            ensure!(
                value.eq_ignore_ascii_case("chunked"),
                "Unsupported HTTP Transfer-Encoding"
            );
            chunked = true;
        }
        headers.push((name.to_string(), value.to_string()));
    }
    ensure!(
        !(chunked && content_length.is_some()),
        "Ambiguous HTTP framing: both Transfer-Encoding and Content-Length present"
    );
    Ok(ParsedHead {
        status,
        headers,
        chunked,
        content_length,
    })
}

async fn read_line_crlf(
    stream: &mut dyn AsyncByteStream,
    pending: &mut Vec<u8>,
    max_len: usize,
    clock: &dyn Clock,
    deadline_ms: u64,
) -> Result<Vec<u8>> {
    let mut scratch = [0u8; 4096];
    loop {
        if let Some(pos) = pending.windows(2).position(|w| w == b"\r\n") {
            ensure!(pos <= max_len, "HTTP chunk line exceeds limit");
            let line = pending[..pos].to_vec();
            pending.drain(..pos + 2);
            return Ok(line);
        }
        ensure!(
            pending.len() <= max_len + 1,
            "HTTP chunk line exceeds limit"
        );
        let Some(n) = read_chunk_into(stream, &mut scratch, clock, deadline_ms).await? else {
            return Err(TransportError::Disconnected.into());
        };
        pending.extend_from_slice(&scratch[..n]);
    }
}

async fn read_exact_buffered(
    stream: &mut dyn AsyncByteStream,
    pending: &mut Vec<u8>,
    len: usize,
    out: &mut Vec<u8>,
    clock: &dyn Clock,
    deadline_ms: u64,
) -> Result<()> {
    let mut need = len;
    if !pending.is_empty() {
        let take = need.min(pending.len());
        out.extend_from_slice(&pending[..take]);
        pending.drain(..take);
        need -= take;
    }
    let mut scratch = [0u8; 16384];
    while need > 0 {
        let step = need.min(scratch.len());
        let Some(n) = read_chunk_into(stream, &mut scratch[..step], clock, deadline_ms).await?
        else {
            return Err(TransportError::Disconnected.into());
        };
        out.extend_from_slice(&scratch[..n]);
        need -= n;
    }
    Ok(())
}

async fn read_chunked_body(
    stream: &mut dyn AsyncByteStream,
    pending: &mut Vec<u8>,
    max_bytes: usize,
    clock: &dyn Clock,
    deadline_ms: u64,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut crlf = Vec::with_capacity(2);
    loop {
        let line =
            read_line_crlf(stream, pending, MAX_CHUNK_LINE_BYTES, clock, deadline_ms).await?;
        let text =
            std::str::from_utf8(&line).map_err(|_| anyhow::anyhow!("Invalid HTTP chunk size"))?;
        let hex_part = text.split(';').next().unwrap_or("").trim();
        ensure!(
            !hex_part.is_empty()
                && hex_part.len() <= 16
                && hex_part.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid HTTP chunk size"
        );
        let chunk_len = usize::from_str_radix(hex_part, 16)
            .map_err(|_| anyhow::anyhow!("HTTP chunk size overflow"))?;
        if chunk_len == 0 {
            let mut trailer_bytes = 0usize;
            let mut trailer_count = 0usize;
            loop {
                let trailer =
                    read_line_crlf(stream, pending, MAX_HTTP_HEADER_BYTES, clock, deadline_ms)
                        .await?;
                if trailer.is_empty() {
                    break;
                }
                trailer_count += 1;
                trailer_bytes = trailer_bytes.saturating_add(trailer.len() + 2);
                ensure!(
                    trailer_count <= MAX_HTTP_HEADERS && trailer_bytes <= MAX_HTTP_HEADER_BYTES,
                    "HTTP chunk trailers exceed limit"
                );
            }
            return Ok(body);
        }
        ensure!(
            chunk_len <= max_bytes.saturating_sub(body.len()),
            TransportError::ResponseTooLarge
        );
        read_exact_buffered(stream, pending, chunk_len, &mut body, clock, deadline_ms).await?;
        crlf.clear();
        read_exact_buffered(stream, pending, 2, &mut crlf, clock, deadline_ms).await?;
        ensure!(crlf == b"\r\n", "Missing CRLF after HTTP chunk");
    }
}

impl<S: StreamTransport> HttpTransport for HttpOverStream<S> {
    fn send<'a>(&'a self, request: HttpRequest) -> TransportFuture<'a, Result<HttpResponse>> {
        Box::pin(async move {
            let (host, port, wire) = format_http_request(&request)?;
            let timeout_ms = u64::try_from(request.timeout.as_millis())
                .unwrap_or(u64::MAX)
                .max(1);
            let deadline_ms = self.clock.monotonic_ms().saturating_add(timeout_ms);
            let connect_timeout = remaining_deadline(&*self.clock, deadline_ms)?;
            let mut stream = self.streams.connect(&host, port, connect_timeout).await?;
            let response: Result<HttpResponse> = async {
                let write_timeout = remaining_deadline(&*self.clock, deadline_ms)?;
                stream.write_all(&wire, write_timeout).await?;

                let mut pending = Vec::new();
                let mut scratch = [0u8; 8192];
                let head_end = loop {
                    if let Some(pos) = find_header_end(&pending) {
                        ensure!(
                            pos <= MAX_HTTP_HEADER_BYTES,
                            "HTTP response headers exceed limit"
                        );
                        break pos;
                    }
                    ensure!(
                        pending.len() <= MAX_HTTP_HEADER_BYTES,
                        "HTTP response headers exceed limit"
                    );
                    let Some(n) =
                        read_chunk_into(&mut *stream, &mut scratch, &*self.clock, deadline_ms)
                            .await?
                    else {
                        return Err(TransportError::Disconnected.into());
                    };
                    pending.extend_from_slice(&scratch[..n]);
                };
                let head = parse_response_head(&pending[..head_end])?;
                pending.drain(..head_end + 4);

                let body = if matches!(head.status, 204 | 304) {
                    Vec::new()
                } else if head.chunked {
                    read_chunked_body(
                        &mut *stream,
                        &mut pending,
                        request.max_response_bytes,
                        &*self.clock,
                        deadline_ms,
                    )
                    .await?
                } else if let Some(length) = head.content_length {
                    ensure!(
                        length <= request.max_response_bytes,
                        TransportError::ResponseTooLarge
                    );
                    let mut body = Vec::with_capacity(length.min(64 * 1024));
                    read_exact_buffered(
                        &mut *stream,
                        &mut pending,
                        length,
                        &mut body,
                        &*self.clock,
                        deadline_ms,
                    )
                    .await?;
                    body
                } else {
                    ensure!(
                        pending.len() <= request.max_response_bytes,
                        TransportError::ResponseTooLarge
                    );
                    let mut body = std::mem::take(&mut pending);
                    let mut buf = [0u8; 16384];
                    while let Some(n) =
                        read_chunk_into(&mut *stream, &mut buf, &*self.clock, deadline_ms).await?
                    {
                        ensure!(
                            n <= request.max_response_bytes.saturating_sub(body.len()),
                            TransportError::ResponseTooLarge
                        );
                        body.extend_from_slice(&buf[..n]);
                    }
                    body
                };
                Ok(HttpResponse {
                    status: head.status,
                    headers: head.headers,
                    body,
                })
            }
            .await;
            let _ = stream.close().await;
            response
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    };

    struct FakeClock {
        monotonic: AtomicU64,
        unix_ms: AtomicU64,
    }
    impl FakeClock {
        fn new(unix_ms: u64) -> Self {
            Self {
                monotonic: AtomicU64::new(1000),
                unix_ms: AtomicU64::new(unix_ms),
            }
        }
    }
    impl Clock for FakeClock {
        fn monotonic_ms(&self) -> u64 {
            self.monotonic.load(Ordering::Acquire)
        }
        fn unix_time_ms(&self) -> Result<u64> {
            Ok(self.unix_ms.load(Ordering::Acquire))
        }
    }

    struct ScriptedStream {
        reads: Vec<ReadOutcomeOrBytes>,
        written: Arc<Mutex<Vec<u8>>>,
        closed: Arc<Mutex<bool>>,
    }
    enum ReadOutcomeOrBytes {
        Data(Vec<u8>),
        Eof,
        TimedOut,
    }
    impl AsyncByteStream for ScriptedStream {
        fn read<'a>(
            &'a mut self,
            buf: &'a mut [u8],
            _timeout: Duration,
        ) -> TransportFuture<'a, Result<ReadOutcome>> {
            Box::pin(async move {
                if self.reads.is_empty() {
                    return Ok(ReadOutcome::Eof);
                }
                match self.reads.remove(0) {
                    ReadOutcomeOrBytes::Data(mut data) => {
                        let n = data.len().min(buf.len());
                        buf[..n].copy_from_slice(&data[..n]);
                        if n < data.len() {
                            self.reads
                                .insert(0, ReadOutcomeOrBytes::Data(data.split_off(n)));
                        }
                        Ok(ReadOutcome::Bytes(n))
                    }
                    ReadOutcomeOrBytes::Eof => Ok(ReadOutcome::Eof),
                    ReadOutcomeOrBytes::TimedOut => Ok(ReadOutcome::TimedOut),
                }
            })
        }
        fn write_all<'a>(
            &'a mut self,
            buf: &'a [u8],
            _timeout: Duration,
        ) -> TransportFuture<'a, Result<()>> {
            Box::pin(async move {
                self.written.lock().unwrap().extend_from_slice(buf);
                Ok(())
            })
        }
        fn close<'a>(&'a mut self) -> TransportFuture<'a, Result<()>> {
            Box::pin(async move {
                *self.closed.lock().unwrap() = true;
                Ok(())
            })
        }
    }

    struct SingleStreamTransport {
        expected: Mutex<Option<(String, u16)>>,
        reads: Mutex<Vec<ReadOutcomeOrBytes>>,
        written: Arc<Mutex<Vec<u8>>>,
        closed: Arc<Mutex<bool>>,
    }
    impl SingleStreamTransport {
        fn new(host: &str, port: u16, reads: Vec<ReadOutcomeOrBytes>) -> Self {
            Self {
                expected: Mutex::new(Some((host.to_string(), port))),
                reads: Mutex::new(reads),
                written: Arc::new(Mutex::new(Vec::new())),
                closed: Arc::new(Mutex::new(false)),
            }
        }
    }
    impl StreamTransport for SingleStreamTransport {
        fn connect<'a>(
            &'a self,
            host: &'a str,
            port: u16,
            _timeout: Duration,
        ) -> TransportFuture<'a, Result<Box<dyn AsyncByteStream>>> {
            let host = host.to_string();
            Box::pin(async move {
                if let Some((eh, ep)) = self.expected.lock().unwrap().as_ref() {
                    ensure!(host == *eh && port == *ep, "unexpected connect target");
                }
                let reads = std::mem::take(&mut *self.reads.lock().unwrap());
                Ok(Box::new(ScriptedStream {
                    reads,
                    written: self.written.clone(),
                    closed: self.closed.clone(),
                }) as Box<dyn AsyncByteStream>)
            })
        }
    }

    #[tokio::test]
    async fn http_over_stream_handles_post_content_length_and_headers() {
        let streams = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![
                ReadOutcomeOrBytes::Data(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Test: ok\r\n\r\nhe".to_vec(),
                ),
                ReadOutcomeOrBytes::Data(b"llo".to_vec()),
            ],
        ));
        let client =
            HttpOverStream::new(streams.clone(), Arc::new(FakeClock::new(1_700_000_000_000)));
        let response = client
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: Url::parse("http://short.weixin.qq.com/mmtls/01020304").unwrap(),
                headers: vec![
                    ("Content-Type".into(), "application/octet-stream".into()),
                    ("Upgrade".into(), "mmtls".into()),
                    ("Connection".into(), "close".into()),
                ],
                body: b"ping".to_vec(),
                timeout: Duration::from_secs(5),
                max_response_bytes: 1024,
            })
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        assert_eq!(response.header("x-test"), Some("ok"));
        assert!(*streams.closed.lock().unwrap());
        let sent = String::from_utf8(streams.written.lock().unwrap().clone()).unwrap();
        assert!(sent.starts_with(
            "POST /mmtls/01020304 HTTP/1.1\r\nHost: short.weixin.qq.com\r\nContent-Length: 4\r\n"
        ));
        assert_eq!(sent.matches("Connection: close").count(), 1);
        assert!(sent.ends_with("\r\n\r\nping"));
    }

    #[tokio::test]
    async fn http_over_stream_decodes_byte_split_chunked_body_and_enforces_budget() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-Trail: 1\r\n\r\n";
        let reads = raw
            .iter()
            .map(|&b| ReadOutcomeOrBytes::Data(vec![b]))
            .collect();
        let streams = Arc::new(SingleStreamTransport::new("203.0.113.10", 443, reads));
        let client = HttpOverStream::new(streams, Arc::new(FakeClock::new(1_700_000_000_000)));
        let response = client
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: Url::parse("http://203.0.113.10:443/download").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 11,
            })
            .await
            .unwrap();
        assert_eq!(response.body, b"hello world");

        let over_reads = vec![ReadOutcomeOrBytes::Data(raw.to_vec())];
        let streams = Arc::new(SingleStreamTransport::new("203.0.113.10", 443, over_reads));
        let client = HttpOverStream::new(streams, Arc::new(FakeClock::new(1_700_000_000_000)));
        assert!(client
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: Url::parse("http://203.0.113.10:443/download").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 10,
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn http_over_stream_rejects_https_smuggling_oversized_and_timeouts() {
        let clock = Arc::new(FakeClock::new(1_700_000_000_000));
        let empty = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![],
        ));
        let client = HttpOverStream::new(empty, clock.clone());
        // Refuses HTTPS rather than downgrading.
        assert!(client
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("https://short.weixin.qq.com/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 1024,
            })
            .await
            .is_err());
        // Refuses CRLF header injection and restricted framing headers.
        for (k, v) in [
            ("X-Bad", "ok\r\nInjected: 1"),
            ("Host", "evil.example"),
            ("Content-Length", "0"),
            ("Transfer-Encoding", "chunked"),
        ] {
            assert!(client
                .send(HttpRequest {
                    method: HttpMethod::Get,
                    url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                    headers: vec![(k.into(), v.into())],
                    body: vec![],
                    timeout: Duration::from_secs(5),
                    max_response_bytes: 1024,
                })
                .await
                .is_err());
        }
        // Rejects conflicting Content-Length + Transfer-Encoding.
        let bad_frames = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 9999\r\n\r\n",
        ];
        for raw in bad_frames {
            let s = Arc::new(SingleStreamTransport::new(
                "short.weixin.qq.com",
                80,
                vec![ReadOutcomeOrBytes::Data(raw.to_vec())],
            ));
            let c = HttpOverStream::new(s, clock.clone());
            assert!(c
                .send(HttpRequest {
                    method: HttpMethod::Get,
                    url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                    headers: vec![],
                    body: vec![],
                    timeout: Duration::from_secs(5),
                    max_response_bytes: 64,
                })
                .await
                .is_err());
        }
        // Timeout classifies as TransportError::Timeout.
        let s = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![ReadOutcomeOrBytes::TimedOut],
        ));
        let c = HttpOverStream::new(s, clock);
        let err = c
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 64,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<TransportError>(),
            Some(&TransportError::Timeout)
        );
    }

    #[tokio::test]
    async fn http_over_stream_reads_close_delimited_body_within_budget() {
        let s = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![
                ReadOutcomeOrBytes::Data(b"HTTP/1.1 200 OK\r\n\r\nabc".to_vec()),
                ReadOutcomeOrBytes::Data(b"def".to_vec()),
                ReadOutcomeOrBytes::Eof,
            ],
        ));
        let c = HttpOverStream::new(s, Arc::new(FakeClock::new(1_700_000_000_000)));
        let resp = c
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 6,
            })
            .await
            .unwrap();
        assert_eq!(resp.body, b"abcdef");
    }

    #[tokio::test]
    async fn http_over_stream_handles_302_204_strict_headers_truncation_and_close_on_error() {
        let clock = Arc::new(FakeClock::new(1_700_000_000_000));

        // 302 must NOT follow Location; only one connection is opened.
        let s302 = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![ReadOutcomeOrBytes::Data(
                b"HTTP/1.1 302 Found\r\nLocation: http://evil.example/\r\nContent-Length: 0\r\n\r\n"
                    .to_vec(),
            )],
        ));
        let resp = HttpOverStream::new(s302.clone(), clock.clone())
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 64,
            })
            .await
            .unwrap();
        assert_eq!(resp.status, 302);
        assert_eq!(resp.header("location"), Some("http://evil.example/"));
        assert!(resp.body.is_empty());

        // 204 No Content returns empty body cleanly without Content-Length.
        let s204 = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![ReadOutcomeOrBytes::Data(
                b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
            )],
        ));
        let resp = HttpOverStream::new(s204, clock.clone())
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 64,
            })
            .await
            .unwrap();
        assert_eq!(resp.status, 204);
        assert!(resp.body.is_empty());

        // Reject port 0.
        let s0 = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![],
        ));
        assert!(HttpOverStream::new(s0, clock.clone())
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("http://short.weixin.qq.com:0/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 64,
            })
            .await
            .is_err());

        // Reject strict HTTP parser edge cases AND verify stream is closed on error.
        let mut oversized_head = b"HTTP/1.1 200 OK\r\nX-Pad: ".to_vec();
        oversized_head.extend(vec![b'a'; MAX_HTTP_HEADER_BYTES + 32]);
        oversized_head.extend_from_slice(b"\r\n\r\n");

        let bad_responses: &[&[u8]] = &[
            b"HTTP/1.1 2000 OK\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200OK\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length : 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nX-Bad: a\nb\r\nContent-Length: 0\r\n\r\n",
            oversized_head.as_slice(),
        ];
        for raw in bad_responses {
            let s = Arc::new(SingleStreamTransport::new(
                "short.weixin.qq.com",
                80,
                vec![ReadOutcomeOrBytes::Data(raw.to_vec())],
            ));
            let err = HttpOverStream::new(s.clone(), clock.clone())
                .send(HttpRequest {
                    method: HttpMethod::Get,
                    url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                    headers: vec![],
                    body: vec![],
                    timeout: Duration::from_secs(5),
                    max_response_bytes: 64,
                })
                .await;
            assert!(err.is_err(), "expected error for malformed HTTP head");
            assert!(
                *s.closed.lock().unwrap(),
                "stream must be closed on response error"
            );
        }

        // Truncated Content-Length EOF classifies as TransportError::Disconnected and closes stream.
        let s_eof = Arc::new(SingleStreamTransport::new(
            "short.weixin.qq.com",
            80,
            vec![
                ReadOutcomeOrBytes::Data(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhi".to_vec(),
                ),
                ReadOutcomeOrBytes::Eof,
            ],
        ));
        let err = HttpOverStream::new(s_eof.clone(), clock)
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                headers: vec![],
                body: vec![],
                timeout: Duration::from_secs(5),
                max_response_bytes: 64,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<TransportError>(),
            Some(&TransportError::Disconnected)
        );
        assert!(*s_eof.closed.lock().unwrap());

        // Oversized Content-Length, chunked, and close-delimited bodies classify as
        // TransportError::ResponseTooLarge and close the stream.
        let oversized_cases: &[&[u8]] = &[
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n41\r\n",
            b"HTTP/1.1 200 OK\r\n\r\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdefX",
        ];
        for raw in oversized_cases {
            let s_big = Arc::new(SingleStreamTransport::new(
                "short.weixin.qq.com",
                80,
                vec![ReadOutcomeOrBytes::Data(raw.to_vec())],
            ));
            let err =
                HttpOverStream::new(s_big.clone(), Arc::new(FakeClock::new(1_700_000_000_000)))
                    .send(HttpRequest {
                        method: HttpMethod::Get,
                        url: Url::parse("http://short.weixin.qq.com/x").unwrap(),
                        headers: vec![],
                        body: vec![],
                        timeout: Duration::from_secs(5),
                        max_response_bytes: 64,
                    })
                    .await
                    .unwrap_err();
            assert_eq!(
                find_transport_error(&err),
                Some(TransportError::ResponseTooLarge)
            );
            assert!(*s_big.closed.lock().unwrap());
        }

        // Arc<T: Clock> delegates Clock directly without dereferencing.
        let arc_clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        assert_eq!(arc_clock.unix_time_secs().unwrap(), 1_700_000_000);
        arc_clock.sleep(Duration::from_millis(1)).await;
    }
}
