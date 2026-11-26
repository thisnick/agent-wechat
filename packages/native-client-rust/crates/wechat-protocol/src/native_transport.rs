//! Default native transports backed by Tokio: TCP byte streams, a pooled
//! HTTP/1.1 client and a task spawner.
//!
//! [`ProtocolTransport::native`] bundles them for [`crate::NativeClient`].
//! Media URLs taken from messages need [`NativeHttpTransport`] built with
//! [`DestinationPolicy::PublicOnly`].
use super::{
    AsyncByteStream, HttpBodySink, HttpMethod, HttpRequest, HttpResponse, HttpResponseHead,
    HttpTransport, ProtocolTransport, ReadOutcome, StreamTransport, SystemClock, TaskSpawner,
    TransportError, TransportFuture,
};
use anyhow::{ensure, Context, Result};
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::Instant,
};

/// Longest a [`NativeHttpTransport`] waits for a TCP connection, within the
/// request's own deadline.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an idle pooled HTTP/1.1 connection stays open for reuse.
const HTTP_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// Idle pooled connections kept per host, so concurrent CDN transfers to one
/// address can each reuse a connection.
const HTTP_POOL_IDLE_PER_HOST: usize = 4;
/// Most addresses a checked host may resolve to.
const MAX_RESOLVED_ADDRESSES: usize = 64;
/// Longest [`AsyncByteStream::close`] waits for the TCP shutdown to be queued.
const CLOSE_TIMEOUT: Duration = Duration::from_millis(250);

impl ProtocolTransport {
    /// Default native transport bundle: [`TokioStreamTransport`] for MMTLS
    /// long links, [`NativeHttpTransport`] with [`DestinationPolicy::System`]
    /// for MMTLS short links, and [`SystemClock`]. Async QR login may answer a
    /// LAN risk challenge with a temporary local listener
    /// ([`Self::with_lan_login`]).
    ///
    /// Requests run on the caller's Tokio runtime, which needs its I/O and
    /// time drivers enabled. Submitted account exchanges detach onto the
    /// current runtime, so dropping a caller's future after submission still
    /// adopts verified replies. Attach [`TokioSpawner`] with
    /// [`Self::with_spawner`] when calls may come from outside that runtime.
    pub fn native() -> Result<Self> {
        Ok(Self::new(
            Arc::new(TokioStreamTransport::new()),
            Arc::new(NativeHttpTransport::new(DestinationPolicy::System)?),
            Arc::new(SystemClock::new()),
        )
        .with_lan_login())
    }
}

/// [`TaskSpawner`] that runs detached account exchanges on a Tokio runtime.
#[derive(Clone, Debug)]
pub struct TokioSpawner {
    handle: tokio::runtime::Handle,
}

impl TokioSpawner {
    /// Spawn onto the runtime that `handle` refers to.
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }

    /// Spawn onto the Tokio runtime the caller is running in. Fails outside
    /// a runtime.
    pub fn current() -> Result<Self> {
        Ok(Self::new(
            tokio::runtime::Handle::try_current().context("No Tokio runtime is running")?,
        ))
    }
}

impl TaskSpawner for TokioSpawner {
    fn spawn(&self, task: TransportFuture<'static, ()>) {
        self.handle.spawn(task);
    }
}

/// [`StreamTransport`] that opens Tokio TCP connections.
///
/// Hosts are resolved with the system resolver, as the blocking MMTLS
/// session does; `wechat-protocol` checks long-link hosts against its
/// allowlist before connecting.
#[derive(Clone, Debug, Default)]
pub struct TokioStreamTransport {
    _private: (),
}

impl TokioStreamTransport {
    /// Construct a TCP stream transport.
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamTransport for TokioStreamTransport {
    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        timeout: Duration,
    ) -> TransportFuture<'a, Result<Box<dyn AsyncByteStream>>> {
        Box::pin(async move {
            ensure!(!timeout.is_zero(), "Invalid connect timeout");
            let deadline = Instant::now() + timeout;
            let addresses =
                tokio::time::timeout_at(deadline, tokio::net::lookup_host((host, port)))
                    .await
                    .map_err(|_| {
                        anyhow::Error::new(TransportError::Timeout)
                            .context("Native connection failed")
                    })?
                    .map_err(|error| {
                        anyhow::Error::new(TransportError::Disconnected)
                            .context(format!("Native connection failed: {error}"))
                    })?;
            let mut last_error = None;
            for address in addresses {
                match tokio::time::timeout_at(deadline, TcpStream::connect(address)).await {
                    Ok(Ok(stream)) => {
                        return Ok(Box::new(TokioByteStream { stream }) as Box<dyn AsyncByteStream>)
                    }
                    Ok(Err(error)) => last_error = Some(error),
                    Err(_) => {
                        return Err(anyhow::Error::new(TransportError::Timeout)
                            .context("Native connection failed"))
                    }
                }
            }
            let message = match last_error {
                Some(error) => format!("Native connection failed: {error}"),
                None => "Native connection failed: host has no addresses".to_owned(),
            };
            Err(anyhow::Error::new(TransportError::Disconnected).context(message))
        })
    }
}

struct TokioByteStream {
    stream: TcpStream,
}

impl AsyncByteStream for TokioByteStream {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
        timeout: Duration,
    ) -> TransportFuture<'a, Result<ReadOutcome>> {
        Box::pin(async move {
            match tokio::time::timeout(timeout, self.stream.read(buf)).await {
                Err(_) => Ok(ReadOutcome::TimedOut),
                Ok(Ok(0)) => Ok(ReadOutcome::Eof),
                Ok(Ok(read)) => Ok(ReadOutcome::Bytes(read)),
                Ok(Err(error)) => Err(anyhow::Error::new(TransportError::Disconnected)
                    .context(format!("Native connection read failed: {error}"))),
            }
        })
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
        timeout: Duration,
    ) -> TransportFuture<'a, Result<()>> {
        Box::pin(async move {
            match tokio::time::timeout(timeout, self.stream.write_all(buf)).await {
                Err(_) => Err(anyhow::Error::new(TransportError::Timeout)
                    .context("Native connection write timed out")),
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(anyhow::Error::new(TransportError::Disconnected)
                    .context(format!("Native connection write failed: {error}"))),
            }
        })
    }

    fn close<'a>(&'a mut self) -> TransportFuture<'a, Result<()>> {
        Box::pin(async move {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.stream.shutdown()).await;
            Ok(())
        })
    }
}

/// Which network addresses a [`NativeHttpTransport`] may connect to.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestinationPolicy {
    /// Connect to whatever the system resolver returns. Use this for WeChat
    /// service endpoints that `wechat-protocol` validates itself: MMTLS
    /// short links on allowlisted hosts and CDN addresses from a
    /// server-signed configuration.
    System,
    /// Require every address a host resolves to, and any IP address written
    /// in the URL, to be a public unicast address, and connect only to those
    /// checked addresses. A refused destination fails with
    /// [`TransportError::DestinationNotAllowed`] before any connection is
    /// made. Use this for media URLs carried in messages: avatars, stickers,
    /// link thumbnails, Enterprise WeChat media and Channels videos.
    PublicOnly,
}

/// [`HttpTransport`] backed by an asynchronous `reqwest` client.
///
/// One instance keeps a pool of HTTP/1.1 keep-alive connections, so share it
/// (for example in an [`Arc`]) across requests to the same hosts. Pooled
/// connections belong to the Tokio runtime that opened them, so use one
/// instance from a single runtime. Requests
/// never use a proxy and never follow redirects; a redirect response is
/// returned to the caller unchanged. Request headers follow the same rules as
/// [`super::HttpOverStream`]: no `Host`, `Content-Length` or
/// `Transfer-Encoding`, and `Connection` only as `close`. Responses are never
/// decompressed. `HttpRequest::timeout` bounds the whole
/// exchange, including the body, and the body is read in pieces so a
/// response larger than `HttpRequest::max_response_bytes` fails with
/// [`TransportError::ResponseTooLarge`] without being held in memory.
pub struct NativeHttpTransport {
    client: reqwest::Client,
    policy: DestinationPolicy,
    allowed: fn(IpAddr) -> bool,
}

impl fmt::Debug for NativeHttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeHttpTransport")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl NativeHttpTransport {
    /// Construct a transport that connects according to `policy`.
    pub fn new(policy: DestinationPolicy) -> Result<Self> {
        Self::with_address_check(policy, crate::media::public_ip)
    }

    /// The destination policy this transport enforces.
    pub fn policy(&self) -> DestinationPolicy {
        self.policy
    }

    fn with_address_check(policy: DestinationPolicy, allowed: fn(IpAddr) -> bool) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .http1_only()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            // Bodies and their framing headers reach the protocol checks unchanged,
            // even when another crate enables reqwest's decompression features.
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .pool_idle_timeout(HTTP_POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(HTTP_POOL_IDLE_PER_HOST);
        builder = match policy {
            DestinationPolicy::System => builder,
            DestinationPolicy::PublicOnly => {
                builder.dns_resolver(Arc::new(CheckedResolver { allowed }))
            }
        };
        Ok(Self {
            client: builder.build().context("HTTP client setup failed")?,
            policy,
            allowed,
        })
    }

    /// Send `request` and stream the response into `sink`, enforcing the
    /// destination policy, the deadline and the receive budget.
    async fn exchange(
        &self,
        request: HttpRequest,
        sink: &mut dyn HttpBodySink,
    ) -> Result<HttpResponseHead> {
        ensure!(
            matches!(request.url.scheme(), "http" | "https"),
            "Unsupported HTTP URL scheme"
        );
        ensure!(!request.timeout.is_zero(), "Invalid HTTP timeout");
        let check_literal = match self.policy {
            DestinationPolicy::System => false,
            DestinationPolicy::PublicOnly => true,
        };
        if check_literal {
            let literal = match request.url.host() {
                Some(url::Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
                Some(url::Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
                Some(url::Host::Domain(_)) => None,
                None => anyhow::bail!("HTTP URL has no host"),
            };
            if literal.is_some_and(|ip| !(self.allowed)(ip)) {
                return Err(anyhow::Error::new(TransportError::DestinationNotAllowed)
                    .context("HTTP destination is not a public address"));
            }
        }
        let deadline = Instant::now() + request.timeout;
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut builder = self
            .client
            .request(method, request.url)
            .timeout(request.timeout);
        for (name, value) in &request.headers {
            super::check_request_header(name, value)?;
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .context("Invalid HTTP request header name")?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .context("Invalid HTTP request header value")?;
            builder = builder.header(name, value);
        }
        if request.method == HttpMethod::Post || !request.body.is_empty() {
            builder = builder.body(request.body);
        }
        let mut response = tokio::time::timeout_at(deadline, builder.send())
            .await
            .map_err(|_| timed_out())?
            .map_err(classify)?;
        let head = HttpResponseHead {
            status: response.status().as_u16(),
            headers: response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect(),
        };
        let limit = request.max_response_bytes;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(too_large());
        }
        sink.head(&head)?;
        let mut received = 0usize;
        while let Some(chunk) = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| timed_out())?
            .map_err(classify)?
        {
            if chunk.len() > limit - received {
                return Err(too_large());
            }
            received += chunk.len();
            if !chunk.is_empty() {
                sink.chunk(&chunk)?;
            }
        }
        Ok(head)
    }
}

/// Collects a whole response for [`HttpTransport::send`].
#[derive(Default)]
struct BufferSink {
    body: Vec<u8>,
}

impl HttpBodySink for BufferSink {
    fn head(&mut self, _head: &HttpResponseHead) -> Result<()> {
        Ok(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> Result<()> {
        self.body.extend_from_slice(bytes);
        Ok(())
    }
}

impl HttpTransport for NativeHttpTransport {
    fn send<'a>(&'a self, request: HttpRequest) -> TransportFuture<'a, Result<HttpResponse>> {
        Box::pin(async move {
            let mut sink = BufferSink::default();
            let head = self.exchange(request, &mut sink).await?;
            Ok(HttpResponse {
                status: head.status,
                headers: head.headers,
                body: sink.body,
            })
        })
    }

    fn send_streaming<'a>(
        &'a self,
        request: HttpRequest,
        sink: &'a mut dyn HttpBodySink,
    ) -> TransportFuture<'a, Result<HttpResponseHead>> {
        Box::pin(self.exchange(request, sink))
    }
}

fn timed_out() -> anyhow::Error {
    anyhow::Error::new(TransportError::Timeout).context("HTTP request timed out")
}

fn too_large() -> anyhow::Error {
    anyhow::Error::new(TransportError::ResponseTooLarge)
        .context("HTTP response body exceeds receive budget")
}

/// Map a `reqwest` failure onto a [`TransportError`] category. The message
/// keeps the cause chain without the URL, which can carry signed media
/// parameters.
fn classify(error: reqwest::Error) -> anyhow::Error {
    let error = error.without_url();
    let mut message = String::from("HTTP request failed");
    let mut refused = false;
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(cause) = source {
        refused |= cause.is::<DestinationRefused>();
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    let category = if refused {
        TransportError::DestinationNotAllowed
    } else if error.is_timeout() {
        TransportError::Timeout
    } else if error.is_builder() {
        return anyhow::Error::new(error).context("Invalid HTTP request");
    } else if error.is_connect() {
        TransportError::Disconnected
    } else {
        // The connection was made, then failed while sending the request or
        // reading the response.
        TransportError::Interrupted
    };
    anyhow::Error::new(category).context(message)
}

/// A host resolved to an address the transport's policy refuses.
#[derive(Debug)]
struct DestinationRefused;

impl fmt::Display for DestinationRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("host resolved to an address that is not public")
    }
}

impl std::error::Error for DestinationRefused {}

/// Resolver for [`DestinationPolicy::PublicOnly`]: the connection is made
/// only to the addresses returned here, after every one of them passed the
/// check.
struct CheckedResolver {
    allowed: fn(IpAddr) -> bool,
}

impl reqwest::dns::Resolve for CheckedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allowed = self.allowed;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .collect::<Vec<_>>();
            let addresses = checked_addresses(resolved, allowed)?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Accept a resolution only when it is nonempty, bounded and entirely
/// allowed, with IPv4 addresses first.
fn checked_addresses(
    mut addresses: Vec<SocketAddr>,
    allowed: fn(IpAddr) -> bool,
) -> std::result::Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    if addresses.is_empty() {
        return Err("host has no addresses".into());
    }
    if addresses.len() > MAX_RESOLVED_ADDRESSES || !addresses.iter().all(|a| allowed(a.ip())) {
        return Err(Box::new(DestinationRefused));
    }
    addresses.sort_by_key(|address| !address.is_ipv4());
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::find_transport_error;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use url::Url;

    fn get(url: &str, max_response_bytes: usize, timeout: Duration) -> HttpRequest {
        HttpRequest {
            method: HttpMethod::Get,
            url: Url::parse(url).unwrap(),
            headers: vec![("Accept-Encoding".into(), "identity".into())],
            body: Vec::new(),
            timeout,
            max_response_bytes,
        }
    }

    /// Serve each accepted connection with `respond`, which reads one request
    /// head and returns the bytes to write (or `None` to stay silent). Counts
    /// accepted connections and handled requests.
    async fn serve(
        respond: impl Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> (SocketAddr, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let respond = Arc::new(respond);
        let (accepted_count, request_count) = (accepted.clone(), requests.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                accepted_count.fetch_add(1, Ordering::SeqCst);
                let respond = respond.clone();
                let request_count = request_count.clone();
                tokio::spawn(async move {
                    let mut pending = Vec::new();
                    loop {
                        let mut chunk = [0u8; 4096];
                        let Ok(read) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        pending.extend_from_slice(&chunk[..read]);
                        while let Some(end) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&pending[..end]).into_owned();
                            let length = head
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if pending.len() < end + 4 + length {
                                break;
                            }
                            pending.drain(..end + 4 + length);
                            request_count.fetch_add(1, Ordering::SeqCst);
                            match respond(&head) {
                                Some(reply) => {
                                    if socket.write_all(&reply).await.is_err() {
                                        return;
                                    }
                                }
                                None => {
                                    tokio::time::sleep(Duration::from_secs(30)).await;
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        (address, accepted, requests)
    }

    fn ok(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Test: one\r\nX-Test: two\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn reuses_one_keep_alive_connection_and_returns_status_headers_and_body() {
        let (address, accepted, requests) = serve(|head| {
            assert!(head.starts_with("POST /cdn HTTP/1.1") || head.starts_with("GET /a HTTP/1.1"));
            Some(ok("hello"))
        })
        .await;
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        let first = http
            .send(get(
                &format!("http://{address}/a"),
                1024,
                Duration::from_secs(5),
            ))
            .await
            .unwrap();
        assert_eq!(first.status, 200);
        assert_eq!(first.body, b"hello");
        assert_eq!(first.header_values("x-test"), vec!["one", "two"]);
        let second = http
            .send(HttpRequest {
                method: HttpMethod::Post,
                url: Url::parse(&format!("http://{address}/cdn")).unwrap(),
                headers: vec![("C-Seq".into(), "2".into())],
                body: b"payload".to_vec(),
                timeout: Duration::from_secs(5),
                max_response_bytes: 1024,
            })
            .await
            .unwrap();
        assert_eq!(second.body, b"hello");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn applies_the_shared_request_header_rules() {
        let (address, accepted, requests) = serve(|head| {
            assert!(head.to_ascii_lowercase().contains("connection: close"));
            Some(ok("closed"))
        })
        .await;
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        for (name, value) in [
            ("Host", "other.example"),
            ("Content-Length", "1"),
            ("Transfer-Encoding", "chunked"),
            ("Connection", "keep-alive"),
        ] {
            let mut request = get(&format!("http://{address}/a"), 1024, Duration::from_secs(5));
            request.headers.push((name.into(), value.into()));
            assert!(http.send(request).await.is_err(), "{name}");
        }
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
        let mut request = get(&format!("http://{address}/a"), 1024, Duration::from_secs(5));
        request.headers.push(("Connection".into(), "close".into()));
        assert_eq!(http.send(request).await.unwrap().body, b"closed");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn streams_bodies_to_a_sink_and_stops_when_it_refuses() {
        let (address, _, _) = serve(|_| {
            Some(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n3\r\nghi\r\n0\r\n\r\n"
                    .to_vec(),
            )
        })
        .await;
        struct Recorder {
            status: Option<u16>,
            pieces: Vec<Vec<u8>>,
            refuse_after: usize,
        }
        impl HttpBodySink for Recorder {
            fn head(&mut self, head: &HttpResponseHead) -> Result<()> {
                self.status = Some(head.status);
                Ok(())
            }
            fn chunk(&mut self, bytes: &[u8]) -> Result<()> {
                self.pieces.push(bytes.to_vec());
                anyhow::ensure!(self.pieces.len() < self.refuse_after, "sink refused");
                Ok(())
            }
        }
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        let mut all = Recorder {
            status: None,
            pieces: Vec::new(),
            refuse_after: usize::MAX,
        };
        let head = http
            .send_streaming(
                get(&format!("http://{address}/a"), 1024, Duration::from_secs(5)),
                &mut all,
            )
            .await
            .unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(all.status, Some(200));
        assert_eq!(all.pieces.concat(), b"abcdefghi");

        let mut refusing = Recorder {
            status: None,
            pieces: Vec::new(),
            refuse_after: 1,
        };
        let error = http
            .send_streaming(
                get(&format!("http://{address}/a"), 1024, Duration::from_secs(5)),
                &mut refusing,
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "sink refused");
        assert_eq!(refusing.pieces.len(), 1);

        let error = http
            .send_streaming(
                get(&format!("http://{address}/a"), 4, Duration::from_secs(5)),
                &mut Recorder {
                    status: None,
                    pieces: Vec::new(),
                    refuse_after: usize::MAX,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            find_transport_error(&error),
            Some(TransportError::ResponseTooLarge)
        );
    }

    #[tokio::test]
    async fn a_connection_lost_mid_body_is_interrupted_not_disconnected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            // Promise ten bytes, send three, then drop the connection.
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
                .await
                .unwrap();
        });
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        let error = http
            .send(get(
                &format!("http://{address}/a"),
                1024,
                Duration::from_secs(5),
            ))
            .await
            .unwrap_err();
        assert_eq!(
            find_transport_error(&error),
            Some(TransportError::Interrupted),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn returns_redirects_without_following_them() {
        let (address, _, requests) = serve(|_| {
            Some(
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/next\r\nContent-Length: 0\r\n\r\n"
                    .to_vec(),
            )
        })
        .await;
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        let response = http
            .send(get(
                &format!("http://{address}/a"),
                1024,
                Duration::from_secs(5),
            ))
            .await
            .unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(response.header("location"), Some("http://127.0.0.1:1/next"));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refuses_bodies_over_the_receive_budget_with_or_without_a_length() {
        let (address, _, _) = serve(|head| {
            Some(if head.starts_with("GET /chunked") {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nabcdef\r\n6\r\nghijkl\r\n0\r\n\r\n"
                    .to_vec()
            } else {
                ok("0123456789")
            })
        })
        .await;
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        for path in ["sized", "chunked"] {
            let error = http
                .send(get(
                    &format!("http://{address}/{path}"),
                    8,
                    Duration::from_secs(5),
                ))
                .await
                .unwrap_err();
            assert_eq!(
                find_transport_error(&error),
                Some(TransportError::ResponseTooLarge),
                "{path}: {error:#}"
            );
        }
        let exact = http
            .send(get(
                &format!("http://{address}/sized"),
                10,
                Duration::from_secs(5),
            ))
            .await
            .unwrap();
        assert_eq!(exact.body, b"0123456789");
    }

    #[tokio::test]
    async fn classifies_silent_servers_as_timeouts_and_refused_connections_as_disconnects() {
        let (address, _, _) = serve(|_| None).await;
        let http = NativeHttpTransport::new(DestinationPolicy::System).unwrap();
        let error = http
            .send(get(
                &format!("http://{address}/a"),
                1024,
                Duration::from_millis(200),
            ))
            .await
            .unwrap_err();
        assert_eq!(find_transport_error(&error), Some(TransportError::Timeout));

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let error = http
            .send(get(
                &format!("http://127.0.0.1:{port}/a"),
                1024,
                Duration::from_secs(5),
            ))
            .await
            .unwrap_err();
        assert_eq!(
            find_transport_error(&error),
            Some(TransportError::Disconnected),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn public_only_refuses_private_literals_and_resolutions_before_connecting() {
        let (address, accepted, _) = serve(|_| Some(ok("secret"))).await;
        let http = NativeHttpTransport::new(DestinationPolicy::PublicOnly).unwrap();
        for url in [
            format!("http://{address}/a"),
            format!("http://localhost:{}/a", address.port()),
        ] {
            let error = http
                .send(get(&url, 1024, Duration::from_secs(5)))
                .await
                .unwrap_err();
            assert_eq!(
                find_transport_error(&error),
                Some(TransportError::DestinationNotAllowed),
                "{url}: {error:#}"
            );
            assert!(!format!("{error:#}").contains("localhost:"));
        }
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn public_only_connects_to_the_checked_resolution() {
        let (address, accepted, _) = serve(|_| Some(ok("checked"))).await;
        let http = NativeHttpTransport::with_address_check(DestinationPolicy::PublicOnly, |ip| {
            ip.is_loopback()
        })
        .unwrap();
        let response = http
            .send(get(
                &format!("http://localhost:{}/a", address.port()),
                1024,
                Duration::from_secs(5),
            ))
            .await
            .unwrap();
        assert_eq!(response.body, b"checked");
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn checked_resolutions_are_bounded_entirely_allowed_and_prefer_ipv4() {
        let v6: SocketAddr = "[2001:4860::1]:0".parse().unwrap();
        let v4: SocketAddr = "8.8.8.8:0".parse().unwrap();
        let private: SocketAddr = "10.0.0.1:0".parse().unwrap();
        let public = crate::media::public_ip;
        assert_eq!(checked_addresses(vec![v6, v4], public).unwrap(), [v4, v6]);
        assert!(checked_addresses(vec![], public).is_err());
        let refused = checked_addresses(vec![v4, private], public).unwrap_err();
        assert!(refused.is::<DestinationRefused>());
        let many = vec![v4; MAX_RESOLVED_ADDRESSES + 1];
        assert!(checked_addresses(many, public)
            .unwrap_err()
            .is::<DestinationRefused>());
        for address in [
            "192.88.99.1",
            "198.18.0.1",
            "100.64.0.1",
            "169.254.1.1",
            "::1",
            "fe80::1",
            "2002::1",
        ] {
            assert!(!public(address.parse().unwrap()), "{address}");
        }
    }

    #[tokio::test]
    async fn stream_transport_reads_writes_and_reports_timeouts_and_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            socket.read_exact(&mut buf).await.unwrap();
            socket.write_all(&buf).await.unwrap();
            // Stay silent so the next read times out, then close.
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
        let transport = TokioStreamTransport::new();
        let mut stream = transport
            .connect("127.0.0.1", port, Duration::from_secs(5))
            .await
            .unwrap();
        stream
            .write_all(b"ping", Duration::from_secs(5))
            .await
            .unwrap();
        let mut buf = [0u8; 16];
        let mut echoed = Vec::new();
        while echoed.len() < 4 {
            match stream.read(&mut buf, Duration::from_secs(5)).await.unwrap() {
                ReadOutcome::Bytes(read) => echoed.extend_from_slice(&buf[..read]),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(echoed, b"ping");
        assert_eq!(
            stream
                .read(&mut buf, Duration::from_millis(50))
                .await
                .unwrap(),
            ReadOutcome::TimedOut
        );
        assert_eq!(
            stream.read(&mut buf, Duration::from_secs(5)).await.unwrap(),
            ReadOutcome::Eof
        );
        stream.close().await.unwrap();

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let error = transport
            .connect("127.0.0.1", port, Duration::from_secs(5))
            .await
            .err()
            .unwrap();
        assert_eq!(
            find_transport_error(&error),
            Some(TransportError::Disconnected)
        );
        // The last per-address cause stays in the message.
        let message = error.to_string();
        assert!(
            message.len() > "Native connection failed: ".len()
                && message.starts_with("Native connection failed: "),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn spawner_runs_tasks_on_the_runtime() {
        let spawner = TokioSpawner::current().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        spawner.spawn(Box::pin(async move {
            tx.send(7).unwrap();
        }));
        assert_eq!(rx.await.unwrap(), 7);
        assert!(std::thread::spawn(|| TokioSpawner::current().is_err())
            .join()
            .unwrap());
        let native = ProtocolTransport::native().unwrap();
        assert!(native.spawner().is_none());
        assert!(native.lan_login());
        let custom = ProtocolTransport::new(
            Arc::new(TokioStreamTransport::new()),
            Arc::new(NativeHttpTransport::new(DestinationPolicy::System).unwrap()),
            Arc::new(SystemClock::new()),
        );
        assert!(!custom.lan_login());
        assert!(custom.with_lan_login().lan_login());
    }
}
