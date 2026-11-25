//! In-band reliable messages on Live96 packed kind `0x50` (relay command `0xd9`).
//!
//! During a call, official clients exchange small control messages (client version,
//! device model, capability flags, direct-connection details) over a reliable-UDP
//! layer carried in `0x50` packets. The sender retransmits each data packet until the
//! receiver acknowledges it with a feedback packet, for up to about 60 seconds.
//!
//! Wire shape (protobuf):
//! - `RudpPkt { 1: head_sign (1 = data, 2 = feedback), 2: version (0), 3: timestamp_ms,
//!   4: content, 5: member_id (receiver) }`
//! - data content: `RudpRawPkt { 1: stream_id, 2: pkt_num, 3: payload }`
//! - feedback content: `RudpFeedback { 1: RudpFeedbackStream { 1: stream_id, 3: max_pkt_num } }`
//!
//! This module parses inbound data packets and builds acknowledgements. It does not
//! interpret message payloads and stores nothing beyond per-call ack state.

use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

const HEAD_DATA: u64 = 1;
const HEAD_FEEDBACK: u64 = 2;
/// Distinct RUDP streams tracked per call.
const MAX_STREAMS: usize = 16;

/// One inbound RUDP data packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RudpData {
    /// `RudpRawPkt` field 1.
    pub stream_id: u32,
    /// `RudpRawPkt` field 2; numbering starts at 1 on each stream.
    pub pkt_num: u64,
    /// `RudpCmd` field 1 when the payload parses as one.
    pub cmd_type: Option<u64>,
    /// Length of the `RudpRawPkt` payload in bytes.
    pub payload_len: usize,
}

/// Parse an authenticated `0x50` payload. Returns `Ok(None)` for feedback packets and
/// any other head sign, which need no acknowledgement.
pub fn parse_rudp_data(payload: &[u8]) -> Result<Option<RudpData>> {
    let outer = p::decode(payload)?;
    if opt_varint(&outer, 1)? != Some(HEAD_DATA) {
        return Ok(None);
    }
    let Some(content) = p::optional_data(&outer, 4)? else {
        bail!("RUDP data packet without content");
    };
    let raw = p::decode(content)?;
    let stream_id = u32::try_from(opt_varint(&raw, 1)?.unwrap_or(0))?;
    let pkt_num = p::scalar(&raw, 2)?;
    let body = p::optional_data(&raw, 3)?.unwrap_or_default();
    let cmd_type = p::decode(body)
        .ok()
        .and_then(|cmd| opt_varint(&cmd, 1).ok().flatten());
    Ok(Some(RudpData {
        stream_id,
        pkt_num,
        cmd_type,
        payload_len: body.len(),
    }))
}

/// Build a feedback packet acknowledging every packet up to `max_pkt_num` on `stream_id`.
pub fn encode_rudp_feedback(
    timestamp_ms: u32,
    stream_id: u32,
    max_pkt_num: u64,
    receiver_member: u16,
) -> Result<Vec<u8>> {
    let stream = p::message(&[p::uint(1, u64::from(stream_id)), p::uint(3, max_pkt_num)])?;
    let feedback = p::bytes(1, &stream);
    p::message(&[
        p::uint(1, HEAD_FEEDBACK),
        p::uint(2, 0),
        p::uint(3, u64::from(timestamp_ms)),
        p::bytes(4, &feedback),
        p::uint(5, u64::from(receiver_member)),
    ])
}

/// Per-call record of the highest RUDP packet number received on each stream.
///
/// Acknowledgements are cumulative up to that number. Packets the peer sent before relay
/// check-in completed never reach us and are not retransmitted, so waiting for a gap-free
/// sequence would leave the peer retransmitting the rest indefinitely. The payloads are
/// not interpreted, so acknowledging a gap loses nothing this client uses.
#[derive(Debug, Default)]
pub struct RudpAckTracker {
    highest: BTreeMap<u32, u64>,
}

impl RudpAckTracker {
    /// Record one data packet and return the packet number to acknowledge on its stream
    /// (the highest seen so far), or `None` when a new stream exceeds the stream limit.
    pub fn record(&mut self, stream_id: u32, pkt_num: u64) -> Option<u64> {
        if !self.highest.contains_key(&stream_id) && self.highest.len() >= MAX_STREAMS {
            return None;
        }
        let highest = self.highest.entry(stream_id).or_insert(pkt_num);
        *highest = (*highest).max(pkt_num);
        Some(*highest)
    }
}

fn opt_varint(fields: &[Field], number: u32) -> Result<Option<u64>> {
    match p::optional(fields, number)? {
        None => Ok(None),
        Some(Value::Varint(v)) => Ok(Some(*v)),
        Some(_) => bail!("Invalid RUDP scalar field {number}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_packet(stream_id: u64, pkt_num: u64, cmd_type: u64) -> Vec<u8> {
        let cmd = [p::uint(1, cmd_type), p::bytes(2, &[1, 2, 3])].concat();
        let raw = [
            p::uint(1, stream_id),
            p::uint(2, pkt_num),
            p::bytes(3, &cmd),
        ]
        .concat();
        [
            p::uint(1, HEAD_DATA),
            p::uint(2, 0),
            p::uint(3, 12_345),
            p::bytes(4, &raw),
            p::uint(5, 1),
        ]
        .concat()
    }

    #[test]
    fn feedback_matches_documented_wire_shape() {
        // Acknowledges packets 1..=5 on stream 0 to member 0 at timestamp 0x1017d4b2.
        let encoded = encode_rudp_feedback(0x1017_d4b2, 0, 5, 0).unwrap();
        assert_eq!(
            encoded,
            [
                0x08, 0x02, 0x10, 0x00, 0x18, 0xb2, 0xa9, 0xdf, 0x80, 0x01, 0x22, 0x06, 0x0a, 0x04,
                0x08, 0x00, 0x18, 0x05, 0x28, 0x00
            ]
        );
    }

    #[test]
    fn parses_data_and_ignores_feedback() {
        let parsed = parse_rudp_data(&data_packet(0, 3, 29)).unwrap().unwrap();
        assert_eq!(parsed.stream_id, 0);
        assert_eq!(parsed.pkt_num, 3);
        assert_eq!(parsed.cmd_type, Some(29));
        assert_eq!(parsed.payload_len, 7);
        let feedback = encode_rudp_feedback(1, 0, 3, 0).unwrap();
        assert_eq!(parse_rudp_data(&feedback).unwrap(), None);
        assert!(parse_rudp_data(&[p::uint(1, HEAD_DATA)].concat()).is_err());
    }

    #[test]
    fn tracker_acknowledges_highest_packet_seen() {
        let mut acks = RudpAckTracker::default();
        // Packets 1..=4 were sent before check-in and never arrive.
        assert_eq!(acks.record(0, 5), Some(5));
        // Late or retransmitted lower packets keep the same acknowledgement.
        assert_eq!(acks.record(0, 2), Some(5));
        assert_eq!(acks.record(0, 6), Some(6));
        // Streams are independent, and a stream numbered from 0 is still acknowledged.
        assert_eq!(acks.record(7, 0), Some(0));
    }

    #[test]
    fn tracker_bounds_streams() {
        let mut acks = RudpAckTracker::default();
        for stream in 0..MAX_STREAMS as u32 {
            assert_eq!(acks.record(stream, 1), Some(1));
        }
        assert_eq!(acks.record(MAX_STREAMS as u32, 1), None);
        assert_eq!(acks.record(0, 2), Some(2));
    }
}
