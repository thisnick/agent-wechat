//! Opt-in (`WX_CALL_AUDIO_TRACE=1`) receive-path counters for direct calls.
//!
//! An inbound datagram can be discarded at several points before audio reaches the
//! decoder: a foreign source or oversized datagram, an unparsable relay header, a
//! failed authentication or member check, the replay window, an unhandled packet
//! kind, or an audio payload shape the parser does not accept. These counters and
//! bounded samples tell a silent peer apart from packets the receive path drops.
//! Nothing is recorded or logged unless the environment variable is set.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{audio_packet::OrdinaryAudio, rudp::RudpData};

const REPORT_INTERVAL: Duration = Duration::from_secs(2);
/// Upper bound on detailed sample lines per category for one call.
const MAX_SAMPLES: u32 = 8;
/// Leading payload bytes included in a sample line.
const SAMPLE_BYTES: usize = 24;

/// Receive-path counters for one direct call; inert unless enabled.
pub(super) struct RxTrace {
    enabled: bool,
    last_report: Instant,
    datagrams: u64,
    foreign: u64,
    bad_header: u64,
    relay: u64,
    open_errors: u64,
    open_dropped: u64,
    kinds: BTreeMap<u8, u64>,
    conn_ids: BTreeMap<u8, u64>,
    audio_ok: u64,
    audio_bad: u64,
    order_bad: u64,
    pcm_frames: u64,
    window_peak: u16,
    rudp_data: u64,
    rudp_acks: u64,
    open_error_samples: u32,
    audio_bad_samples: u32,
    audio_ok_samples: u32,
    other_kind_samples: u32,
    rudp_data_samples: u32,
    rudp_ack_samples: u32,
    relay_qos_samples: u32,
}

impl RxTrace {
    /// Enable tracing when `WX_CALL_AUDIO_TRACE=1`.
    pub(super) fn from_env() -> Self {
        Self::new(std::env::var("WX_CALL_AUDIO_TRACE").as_deref() == Ok("1"))
    }

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last_report: Instant::now(),
            datagrams: 0,
            foreign: 0,
            bad_header: 0,
            relay: 0,
            open_errors: 0,
            open_dropped: 0,
            kinds: BTreeMap::new(),
            conn_ids: BTreeMap::new(),
            audio_ok: 0,
            audio_bad: 0,
            order_bad: 0,
            pcm_frames: 0,
            window_peak: 0,
            rudp_data: 0,
            rudp_acks: 0,
            open_error_samples: 0,
            audio_bad_samples: 0,
            audio_ok_samples: 0,
            other_kind_samples: 0,
            rudp_data_samples: 0,
            rudp_ack_samples: 0,
            relay_qos_samples: 0,
        }
    }

    /// A datagram from the selected relay endpoint within the size limit. Live96
    /// datagrams also count their cleartext connection-ID byte (header byte 18).
    pub(super) fn datagram(&mut self, packet: &[u8]) {
        if !self.enabled {
            return;
        }
        self.datagrams += 1;
        if packet.first() == Some(&0x96) {
            if let Some(&conn) = packet.get(18) {
                *self.conn_ids.entry(conn).or_default() += 1;
            }
        }
    }

    /// A datagram ignored because it came from another source or was oversized.
    pub(super) fn foreign(&mut self) {
        if self.enabled {
            self.foreign += 1;
        }
    }

    /// A datagram whose cleartext relay header could not be parsed.
    pub(super) fn bad_header(&mut self) {
        if self.enabled {
            self.bad_header += 1;
        }
    }

    /// A datagram from the relay itself (member `0xffff`).
    pub(super) fn relay(&mut self) {
        if self.enabled {
            self.relay += 1;
        }
    }

    /// An authenticated relay QoS payload; the first few are dumped in full so their
    /// sections (for example audio and video S2P) can be read.
    pub(super) fn relay_qos(&mut self, packed_kind: u8, payload: &[u8]) {
        if !self.enabled || self.relay_qos_samples >= 4 {
            return;
        }
        self.relay_qos_samples += 1;
        let mut hex = String::with_capacity(payload.len().min(512) * 2);
        for byte in &payload[..payload.len().min(512)] {
            let _ = write!(hex, "{byte:02x}");
        }
        eprintln!(
            "[wechat-call-audio] rx relay_qos #{} kind=0x{packed_kind:02x} len={} body={hex}",
            self.relay_qos_samples,
            payload.len()
        );
    }

    /// A peer datagram that failed authentication or the room/member checks.
    pub(super) fn open_error(&mut self, packet: &[u8], error: &anyhow::Error) {
        if !self.enabled {
            return;
        }
        self.open_errors += 1;
        if self.open_error_samples < MAX_SAMPLES {
            self.open_error_samples += 1;
            let magic = packet.first().copied().unwrap_or(0);
            let member = packet
                .get(10..12)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            let kind = packet.get(14).copied().unwrap_or(0);
            eprintln!(
                "[wechat-call-audio] rx open_err #{} magic=0x{magic:02x} member={member} kind=0x{kind:02x} len={} err={error:#}",
                self.open_error_samples,
                packet.len()
            );
        }
    }

    /// An authenticated duplicate or a packet outside the replay window.
    pub(super) fn open_dropped(&mut self) {
        if self.enabled {
            self.open_dropped += 1;
        }
    }

    /// An authenticated peer packet of `packed_kind`.
    pub(super) fn opened(&mut self, packed_kind: u8) {
        if self.enabled {
            *self.kinds.entry(packed_kind).or_default() += 1;
        }
    }

    /// An authenticated packet kind the direct receive loop does not handle.
    pub(super) fn other_kind(&mut self, packed_kind: u8, payload: &[u8]) {
        if !self.enabled || self.other_kind_samples >= MAX_SAMPLES {
            return;
        }
        self.other_kind_samples += 1;
        eprintln!(
            "[wechat-call-audio] rx unhandled #{} kind=0x{packed_kind:02x} len={} head={}",
            self.other_kind_samples,
            payload.len(),
            hex_prefix(payload)
        );
    }

    /// An inbound `0x50` RUDP data packet.
    pub(super) fn rudp_data(&mut self, data: &RudpData) {
        if !self.enabled {
            return;
        }
        self.rudp_data += 1;
        if self.rudp_data_samples < 2 * MAX_SAMPLES {
            self.rudp_data_samples += 1;
            eprintln!(
                "[wechat-call-rudp] rx data stream={} pkt={} cmd={:?} len={}",
                data.stream_id, data.pkt_num, data.cmd_type, data.payload_len
            );
        }
    }

    /// A `0x50` RUDP feedback acknowledgement sent to the peer.
    pub(super) fn rudp_ack(&mut self, stream_id: u32, acked: u64) {
        if !self.enabled {
            return;
        }
        self.rudp_acks += 1;
        if self.rudp_ack_samples < 2 * MAX_SAMPLES {
            self.rudp_ack_samples += 1;
            eprintln!("[wechat-call-rudp] tx ack stream={stream_id} max_pkt={acked}");
        }
    }

    /// A `0x10` payload the ordinary95 parser accepted.
    pub(super) fn audio_parsed(&mut self, payload: &[u8], audio: &OrdinaryAudio<'_>) {
        if !self.enabled {
            return;
        }
        self.audio_ok += 1;
        if self.audio_ok_samples < 3 {
            self.audio_ok_samples += 1;
            let mut primaries = String::new();
            for (n, primary) in audio.primaries.iter().enumerate() {
                let _ = write!(
                    primaries,
                    "{}{}b/toc=0x{:02x}",
                    if n == 0 { "" } else { "," },
                    primary.len(),
                    primary.first().copied().unwrap_or(0)
                );
            }
            eprintln!(
                "[wechat-call-audio] rx audio #{} len={} meta={:02x?} seq={} ts={} primaries=[{primaries}]",
                self.audio_ok_samples,
                payload.len(),
                audio.metadata,
                audio.last_sequence,
                audio.last_timestamp
            );
        }
    }

    /// A `0x10` payload the ordinary95 parser rejected.
    pub(super) fn audio_unparsed(&mut self, payload: &[u8], error: &anyhow::Error) {
        if !self.enabled {
            return;
        }
        self.audio_bad += 1;
        if self.audio_bad_samples < MAX_SAMPLES {
            self.audio_bad_samples += 1;
            eprintln!(
                "[wechat-call-audio] rx audio_bad #{} len={} err={error:#} head={}",
                self.audio_bad_samples,
                payload.len(),
                hex_prefix(payload)
            );
        }
    }

    /// An audio packet the RTP order check rejected.
    pub(super) fn order_rejected(&mut self) {
        if self.enabled {
            self.order_bad += 1;
        }
    }

    /// One decoded PCM frame handed to the host.
    pub(super) fn pcm(&mut self, pcm: &[i16]) {
        if !self.enabled {
            return;
        }
        self.pcm_frames += 1;
        let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        self.window_peak = self.window_peak.max(peak);
    }

    /// A decoder failure, which ends the media loop.
    pub(super) fn decode_error(&mut self, packet: &[u8], error: &dyn std::fmt::Display) {
        if self.enabled {
            eprintln!(
                "[wechat-call-audio] rx decode_err len={} err={error} head={}",
                packet.len(),
                hex_prefix(packet)
            );
        }
    }

    /// Print cumulative counters at most once per [`REPORT_INTERVAL`].
    pub(super) fn report(&mut self) {
        if self.enabled && self.last_report.elapsed() >= REPORT_INTERVAL {
            self.print("rx");
        }
    }

    /// Print final counters and how the media loop ended.
    pub(super) fn finish(&mut self, outcome: Option<&anyhow::Error>) {
        if !self.enabled {
            return;
        }
        self.print("end");
        if let Some(error) = outcome {
            eprintln!("[wechat-call-audio] end err={error:#}");
        }
    }

    fn print(&mut self, label: &str) {
        let mut conns = String::new();
        for (n, (conn, count)) in self.conn_ids.iter().enumerate() {
            let _ = write!(conns, "{}{conn}:{count}", if n == 0 { "" } else { "," });
        }
        let mut kinds = String::new();
        for (n, (kind, count)) in self.kinds.iter().enumerate() {
            let _ = write!(
                kinds,
                "{}0x{kind:02x}:{count}",
                if n == 0 { "" } else { "," }
            );
        }
        eprintln!(
            "[wechat-call-audio] {label} dgrams={} foreign={} bad_hdr={} relay={} open_err={} open_drop={} conns=[{conns}] kinds=[{kinds}] audio_ok={} audio_bad={} order_bad={} pcm={} peak={} rudp_rx={} rudp_ack={}",
            self.datagrams,
            self.foreign,
            self.bad_header,
            self.relay,
            self.open_errors,
            self.open_dropped,
            self.audio_ok,
            self.audio_bad,
            self.order_bad,
            self.pcm_frames,
            self.window_peak,
            self.rudp_data,
            self.rudp_acks
        );
        self.window_peak = 0;
        self.last_report = Instant::now();
    }
}

fn hex_prefix(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(SAMPLE_BYTES * 2);
    for byte in &bytes[..bytes.len().min(SAMPLE_BYTES)] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_trace_records_nothing() {
        let mut trace = RxTrace::new(false);
        trace.datagram(&[0x96; 19]);
        trace.opened(0x10);
        trace.pcm(&[100, -200]);
        assert_eq!(trace.datagrams, 0);
        assert!(trace.kinds.is_empty());
        assert_eq!(trace.pcm_frames, 0);
    }

    #[test]
    fn enabled_trace_counts_kinds_and_peak() {
        let mut trace = RxTrace::new(true);
        trace.datagram(&[0x96; 19]);
        trace.opened(0x10);
        trace.opened(0x10);
        trace.opened(0x21);
        trace.pcm(&[100, -32768, 5]);
        assert_eq!(trace.datagrams, 1);
        assert_eq!(trace.conn_ids.get(&0x96), Some(&1));
        assert_eq!(trace.kinds.get(&0x10), Some(&2));
        assert_eq!(trace.kinds.get(&0x21), Some(&1));
        assert_eq!(trace.pcm_frames, 1);
        assert_eq!(trace.window_peak, 32768);
    }

    #[test]
    fn hex_prefix_is_bounded() {
        let bytes = [0xabu8; 40];
        assert_eq!(hex_prefix(&bytes).len(), SAMPLE_BYTES * 2);
        assert_eq!(hex_prefix(&[0x01, 0x95]), "0195");
    }
}
