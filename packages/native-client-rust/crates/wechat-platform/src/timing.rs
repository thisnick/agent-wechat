//! Optional, best-effort timings. Records accept static code labels and numbers,
//! never account identifiers, paths, SQL, message bodies or error strings.
use std::{
    panic::Location,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_RECORDS: usize = 20_000;
const MAX_DURATION: Duration = Duration::from_secs(30 * 60);
const SLOW: Duration = Duration::from_millis(100);
static TRACE: OnceLock<Option<Trace>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct Trace {
    sender: SyncSender<Record>,
    records: AtomicUsize,
    dropped: Arc<AtomicUsize>,
    started: Instant,
    duration: Duration,
}

struct Record {
    id: u64,
    operation: &'static str,
    event: &'static str,
    site: &'static Location<'static>,
    thread: std::thread::ThreadId,
    timestamp_ms: u128,
    elapsed_us: u128,
    bytes: u64,
    items: usize,
}

impl Trace {
    fn emit(&self, record: Record) {
        if self.started.elapsed() >= self.duration
            || self
                .records
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    (n < MAX_RECORDS).then_some(n + 1)
                })
                .is_err()
        {
            return;
        }
        if self.sender.try_send(record).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn trace() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            if std::env::var_os("AGENT_WECHAT_TRACE_TIMINGS").as_deref()
                != Some(std::ffi::OsStr::new("1"))
            {
                return None;
            }
            let (sender, receiver) = sync_channel::<Record>(256);
            let dropped = Arc::new(AtomicUsize::new(0));
            let writer_dropped = dropped.clone();
            std::thread::Builder::new()
                .name("wx-timing".into())
                .spawn(move || {
                    use std::io::Write;
                    while let Ok(r) = receiver.recv() {
                        // The filename is from the compiled call site, not a profile path.
                        let file = r.site.file().rsplit(['/', '\\']).next().unwrap_or("");
                        let _ = writeln!(std::io::stderr(), "wx_timing timestamp_ms={} span={} event={} operation={} thread={:?} site={}:{} elapsed_us={} bytes={} items={} dropped={}", r.timestamp_ms, r.id, r.event, r.operation, r.thread, file, r.site.line(), r.elapsed_us, r.bytes, r.items, writer_dropped.load(Ordering::Relaxed));
                    }
                })
                .ok()?;
            Some(Trace {
                sender,
                records: AtomicUsize::new(0),
                dropped,
                started: Instant::now(),
                duration: MAX_DURATION,
            })
        })
        .as_ref()
}

/// Timing for a fixed code operation. Logging is disabled unless opted in at
/// process startup, expires after 30 minutes, and never waits for the log writer.
pub struct Timing {
    trace: Option<&'static Trace>,
    id: u64,
    operation: &'static str,
    site: &'static Location<'static>,
    started: Instant,
    slow_only: bool,
    bytes: u64,
    items: usize,
}

impl Timing {
    #[track_caller]
    pub fn start(operation: &'static str) -> Self {
        Self::new(operation, false)
    }

    #[track_caller]
    pub fn slow(operation: &'static str) -> Self {
        Self::new(operation, true)
    }

    #[track_caller]
    fn new(operation: &'static str, slow_only: bool) -> Self {
        let trace = trace().filter(|t| {
            t.started.elapsed() < t.duration && t.records.load(Ordering::Relaxed) < MAX_RECORDS
        });
        let timing = Self {
            trace,
            id: if trace.is_some() {
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            } else {
                0
            },
            operation,
            site: Location::caller(),
            started: Instant::now(),
            slow_only,
            bytes: 0,
            items: 0,
        };
        if !slow_only {
            timing.emit("begin");
        }
        timing
    }

    pub fn counts(&mut self, bytes: u64, items: usize) {
        self.bytes = bytes;
        self.items = items;
    }

    fn emit(&self, event: &'static str) {
        if let Some(trace) = self.trace {
            trace.emit(Record {
                id: self.id,
                operation: self.operation,
                event,
                site: self.site,
                thread: std::thread::current().id(),
                timestamp_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                elapsed_us: self.started.elapsed().as_micros(),
                bytes: self.bytes,
                items: self.items,
            });
        }
    }
}

impl Drop for Timing {
    fn drop(&mut self) {
        if !self.slow_only || self.started.elapsed() >= SLOW {
            self.emit(if self.slow_only { "slow" } else { "end" });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record {
            id: 1,
            operation: "synthetic",
            event: "begin",
            site: Location::caller(),
            thread: std::thread::current().id(),
            timestamp_ms: 0,
            elapsed_us: 0,
            bytes: 0,
            items: 0,
        }
    }

    #[test]
    fn full_log_queue_drops_records_without_waiting_for_io() {
        let (sender, receiver) = sync_channel(1);
        let trace = Trace {
            sender,
            records: AtomicUsize::new(0),
            dropped: Arc::new(AtomicUsize::new(0)),
            started: Instant::now(),
            duration: MAX_DURATION,
        };
        trace.emit(record());
        trace.emit(record());
        assert_eq!(trace.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.try_recv().unwrap().id, 1);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn trace_stops_at_record_and_time_limits() {
        let (sender, receiver) = sync_channel(1);
        let mut trace = Trace {
            sender,
            records: AtomicUsize::new(MAX_RECORDS),
            dropped: Arc::new(AtomicUsize::new(0)),
            started: Instant::now(),
            duration: MAX_DURATION,
        };
        trace.emit(record());
        assert!(receiver.try_recv().is_err());
        trace.records.store(0, Ordering::Relaxed);
        trace.duration = Duration::ZERO;
        trace.emit(record());
        assert!(receiver.try_recv().is_err());
        assert_eq!(trace.records.load(Ordering::Relaxed), 0);
    }
}
