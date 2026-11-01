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
    fn reordered_b_before_a_never_rewinds_decoder_input() {
        let mut order = RtpOrder::new();
        // B contains primaries 2,3. A later arrives with 0,1.
        assert_eq!(
            order.admit(3, 960, 2),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
        assert_eq!(
            order.admit(1, 320, 2),
            Ok(PrimaryAdmission { skip: 2, gap: 0 })
        );
        assert_eq!(order.last, Some((3, 960)));
        assert_eq!(
            order.admit(5, 1600, 2),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
    }

    #[test]
    fn fresh_envelope_cannot_replay_an_admitted_rtp_pair() {
        let mut order = RtpOrder::new();
        order.admit(81, 90_000, 2).unwrap();
        assert_eq!(
            order.admit(81, 90_000, 2),
            Ok(PrimaryAdmission { skip: 2, gap: 0 })
        );
        // A changed timestamp does not make the same primaries new.
        assert_eq!(
            order.admit(81, 91_000, 2),
            Ok(PrimaryAdmission { skip: 2, gap: 0 })
        );
        assert_eq!(order.last, Some((81, 90_000)));
    }

    #[test]
    fn partial_overlap_admits_only_the_new_suffix() {
        let mut order = RtpOrder::new();
        order.admit(100, 10, 2).unwrap();
        // Packet 99,100,101 contributes only 101.
        assert_eq!(
            order.admit(101, 20, 3),
            Ok(PrimaryAdmission { skip: 2, gap: 0 })
        );
        // Packet 101,102,103 contributes 102,103.
        assert_eq!(
            order.admit(103, 30, 3),
            Ok(PrimaryAdmission { skip: 1, gap: 0 })
        );
        assert_eq!(order.last, Some((103, 30)));
    }

    #[test]
    fn reports_missing_primaries_without_hiding_them_in_sequence_compression() {
        let mut order = RtpOrder::new();
        order.admit(10, 100, 2).unwrap();
        // Packet 14,15 follows 10: 11,12,13 are missing.
        assert_eq!(
            order.admit(15, 200, 2),
            Ok(PrimaryAdmission { skip: 0, gap: 3 })
        );
        assert_eq!(
            order.admit(16, 300, 1),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
    }

    #[test]
    fn sequence_wrap_keeps_late_and_overlapping_primaries_distinct() {
        let mut order = RtpOrder::new();
        // First packet has the arbitrary origin 65534,65535.
        order.admit(65535, u32::MAX - 320, 2).unwrap();
        assert_eq!(
            order.admit(1, 319, 2),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
        assert_eq!(
            order.admit(65535, u32::MAX - 320, 2),
            Ok(PrimaryAdmission { skip: 2, gap: 0 })
        );
        assert_eq!(
            order.admit(2, 639, 2),
            Ok(PrimaryAdmission { skip: 1, gap: 0 })
        );
        assert_eq!(order.last, Some((2, 639)));

        let mut gap = RtpOrder::new();
        gap.admit(65534, 0, 1).unwrap();
        // Missing 65535,0 precede the newly admitted 1,2.
        assert_eq!(gap.admit(2, 1, 2), Ok(PrimaryAdmission { skip: 0, gap: 2 }));
    }

    #[test]
    fn arbitrary_timestamps_are_metadata_not_an_inferred_ordering_contract() {
        let mut order = RtpOrder::new();
        order.admit(500, 12345, 2).unwrap();
        // Neither a fixed increment, a zero origin nor strictly increasing
        // timestamp values are inferred from the selected header alone.
        assert_eq!(
            order.admit(501, 12345, 1),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
        assert_eq!(
            order.admit(502, 7, 1),
            Ok(PrimaryAdmission { skip: 0, gap: 0 })
        );
        assert_eq!(order.last, Some((502, 7)));
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
