//! Direct, main-context roster transitions. Callers first authenticate a whole
//! newer roster, preserve original identities and exclude escalation contexts.
use super::personal::RemoteExitReason;
use anyhow::{bail, ensure, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalStopReason {
    SelfRemoved,
    HandledElsewhere { sdk_end: u32 },
    PeerLeftBeforeJoin,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteTermination {
    LocalStop(LocalStopReason),
    Exit(RemoteExitReason),
    NoResponseCancel,
}
#[derive(Clone, Copy)]
pub(super) enum Origin {
    Caller,
    Callee,
}

/// `None` is absence from the authenticated full list, never a fabricated row.
/// Device-type changes and timestamps are not takeover evidence.
pub(super) fn classify(
    origin: Origin,
    local_status: u32,
    own: Option<u32>,
    peer: Option<u32>,
    pending_invitation: bool,
) -> Result<Option<RemoteTermination>> {
    ensure!(
        own.is_none_or(|status| (1..=10).contains(&status))
            && peer.is_none_or(|status| (1..=10).contains(&status)),
        "Unsupported call member status"
    );
    if local_status == 1 && own != Some(1) {
        return Ok(Some(RemoteTermination::LocalStop(
            LocalStopReason::SelfRemoved,
        )));
    }
    match origin {
        Origin::Caller => ensure!(local_status == 1, "Unsupported caller local state"),
        Origin::Callee => {
            ensure!(
                (1..=3).contains(&local_status),
                "Unsupported callee local state"
            );
            if local_status != 1 {
                if let Some(status @ (1 | 4 | 6 | 7 | 8)) = own {
                    let sdk_end = match status {
                        1 => 13,
                        6 => 15,
                        7 => 14,
                        _ => 8,
                    };
                    return Ok(Some(RemoteTermination::LocalStop(
                        LocalStopReason::HandledElsewhere { sdk_end },
                    )));
                }
                ensure!(
                    matches!(own, Some(2 | 3)),
                    "Unsupported pending callee membership"
                );
            }
        }
    }
    if matches!(peer, Some(1..=3)) {
        return Ok(None);
    }
    if matches!(origin, Origin::Callee) && local_status != 1 {
        return Ok(Some(RemoteTermination::LocalStop(
            LocalStopReason::PeerLeftBeforeJoin,
        )));
    }
    let reason = match (origin, peer) {
        (Origin::Caller, Some(6)) => RemoteExitReason::PeerBusy,
        (Origin::Caller, Some(7)) => RemoteExitReason::PeerRejected,
        (Origin::Caller, Some(8)) => {
            ensure!(
                pending_invitation,
                "Missing pending invitation for no-response cancellation"
            );
            return Ok(Some(RemoteTermination::NoResponseCancel));
        }
        (_, None | Some(4..=10)) => RemoteExitReason::LastPeerLeft,
        _ => bail!("Unsupported remote member transition"),
    };
    Ok(Some(RemoteTermination::Exit(reason)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_callee_handles_elsewhere_only_for_source_predicate() {
        for local in [2, 3] {
            for (own, end) in [(1, 13), (4, 8), (6, 15), (7, 14), (8, 8)] {
                assert_eq!(
                    classify(Origin::Callee, local, Some(own), Some(1), false).unwrap(),
                    Some(RemoteTermination::LocalStop(
                        LocalStopReason::HandledElsewhere { sdk_end: end }
                    ))
                );
            }
        }
        assert!(classify(Origin::Callee, 3, Some(5), Some(1), false).is_err());
        assert_eq!(
            classify(Origin::Callee, 3, Some(3), Some(1), false).unwrap(),
            None
        );
    }
    #[test]
    fn unsupported_status_or_origin_does_not_become_terminal_proof() {
        assert!(classify(Origin::Caller, 1, Some(1), Some(11), true).is_err());
        assert!(classify(Origin::Callee, 3, None, Some(1), false).is_err());
        assert!(classify(Origin::Caller, 3, Some(3), Some(4), true).is_err());
    }
}
