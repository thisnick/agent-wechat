//! Drop-late ordering for authenticated ordinary95 audio primaries.
//!
//! The header identifies the last primary in a packet. Sequence arithmetic is
//! modular, and the first observed origin may be any value. This is not a jitter
//! buffer: once a newer primary has been admitted, older ones are skipped.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PrimaryAdmission {
    /// Leading primaries that are duplicate or older than the high-water mark.
    pub skip: usize,
    /// Missing primaries before the first newly admitted primary in this packet.
    pub gap: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RtpOrderError {
    InvalidPrimaryCount,
    AmbiguousSequence,
}

impl fmt::Display for RtpOrderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPrimaryCount => "unsupported RTP primary count",
            Self::AmbiguousSequence => "ambiguous RTP sequence distance",
        })
    }
}

impl Error for RtpOrderError {}

#[derive(Default)]
pub(super) struct RtpOrder {
    // Retain the last admitted timestamp as metadata. Ordering does not infer a
    // fixed relationship between sequence distance and timestamp advancement.
    last: Option<(u16, u32)>,
}

impl RtpOrder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call only after transport authentication and ordinary95 extent checks.
    /// Feed `primaries.iter().skip(admission.skip)` to the decoder in order.
    ///
    /// Admission reserves the high-water mark before decoding. A codec failure
    /// does not authorize replaying that primary into an already advanced
    /// decoder. Invalid counts and an exact half-range ambiguity leave state
    /// unchanged; the caller should drop that packet rather than reset order.
    /// A new call/media epoch requires a new instance, never an inferred reset.
    pub fn admit(
        &mut self,
        last_sequence: u16,
        last_timestamp: u32,
        primary_count: usize,
    ) -> Result<PrimaryAdmission, RtpOrderError> {
        if !(1..=3).contains(&primary_count) {
            return Err(RtpOrderError::InvalidPrimaryCount);
        }
        let Some((previous_sequence, _)) = self.last else {
            self.last = Some((last_sequence, last_timestamp));
            return Ok(PrimaryAdmission { skip: 0, gap: 0 });
        };
        let distance = last_sequence.wrapping_sub(previous_sequence);
        if distance == 1 << 15 {
            return Err(RtpOrderError::AmbiguousSequence);
        }
        if distance == 0 || distance > 1 << 15 {
            return Ok(PrimaryAdmission {
                skip: primary_count,
                gap: 0,
            });
        }

        let admission = PrimaryAdmission {
            skip: primary_count.saturating_sub(usize::from(distance)),
            gap: distance.saturating_sub(primary_count as u16),
        };
        self.last = Some((last_sequence, last_timestamp));
        Ok(admission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_sequence_trace_drops_replay_overlap_and_loss_without_rewinding() {
        let mut order = RtpOrder::new();
        // Arbitrary origin, wrap, late packet, replay with a new timestamp,
        // overlapping primaries, a loss gap, then ordinary forward progress.
        for (sequence, timestamp, count, skip, gap) in [
            (65534, u32::MAX - 320, 2, 0, 0),
            (0, 319, 2, 0, 0),
            (65534, 0, 2, 2, 0),
            (0, 999, 2, 2, 0),
            (1, 0, 3, 2, 0),
            (3, 0, 3, 1, 0),
            (8, 7, 2, 0, 3),
            (6, 100, 2, 2, 0),
            (9, 0, 1, 0, 0),
        ] {
            assert_eq!(
                order.admit(sequence, timestamp, count),
                Ok(PrimaryAdmission { skip, gap })
            );
        }
        assert_eq!(order.last, Some((9, 0)));
        // A new call explicitly starts its own ordering epoch; loss across wrap
        // must still be reported rather than hidden by sequence compression.
        let mut next_call = RtpOrder::new();
        next_call.admit(65534, 0, 1).unwrap();
        assert_eq!(
            next_call.admit(2, 1, 2),
            Ok(PrimaryAdmission { skip: 0, gap: 2 })
        );
    }
    #[test]
    fn malformed_count_and_half_range_ambiguity_leave_high_water_unchanged() {
        let mut order = RtpOrder::new();
        for count in [0, 4, usize::MAX] {
            assert_eq!(
                order.admit(9, 10, count),
                Err(RtpOrderError::InvalidPrimaryCount)
            );
            assert_eq!(order.last, None);
        }
        order.admit(5, 10, 2).unwrap();
        for count in [0, 4, usize::MAX] {
            assert_eq!(
                order.admit(7, 20, count),
                Err(RtpOrderError::InvalidPrimaryCount)
            );
            assert_eq!(order.last, Some((5, 10)));
        }
        assert_eq!(
            order.admit(32773, 20, 2),
            Err(RtpOrderError::AmbiguousSequence)
        );
        assert_eq!(order.last, Some((5, 10)));
        assert_eq!(
            order.admit(7, 20, 2),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
    }
}
