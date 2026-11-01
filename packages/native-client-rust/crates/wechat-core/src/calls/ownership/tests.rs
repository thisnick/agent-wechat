use super::*;
fn authority(account: &str) -> AccountAuthority {
    AccountAuthority {
        account_id: account.into(),
        generation: "generation1".into(),
    }
}
fn client(h: &mut Ownership, id: &str, account: &str) {
    h.connect(id, authority(account), "principal").unwrap();
    h.ready(id, true).unwrap();
}
fn identity(h: &Ownership) -> CallIdentity {
    let s = h.session.as_ref().unwrap();
    CallIdentity {
        attempt: s.attempt,
        call_id: s.id.clone(),
    }
}
fn submit(d: Decision) -> Ticket {
    match d {
        Decision::Submit(t) => t,
        _ => panic!("must be unique"),
    }
}
#[test]
fn late_client_discovers_offer_and_first_answer_reserves_before_io() {
    let mut h = Ownership::default();
    h.offer(authority("a"), "call1", 1000, 0).unwrap();
    client(&mut h, "one", "a");
    client(&mut h, "two", "a");
    client(&mut h, "other", "b");
    assert!(h.snapshot("one").unwrap().current.is_some());
    assert_eq!(
        h.snapshot("other").unwrap(),
        Snapshot {
            busy: true,
            current: None
        }
    );
    let ticket = submit(
        h.act("one", "r1", "call1", 1, Operation::Answer, 1)
            .unwrap(),
    );
    assert_eq!(
        h.act("two", "r2", "call1", 1, Operation::Answer, 1),
        Err(Error::Stale)
    );
    assert_eq!(
        h.snapshot("two").unwrap(),
        Snapshot {
            busy: true,
            current: None
        }
    );
    h.result(&ticket, Some(Phase::Connected)).unwrap();
    assert_eq!(
        h.snapshot("one").unwrap().current.unwrap().phase,
        Phase::Connected
    );
}
#[test]
fn outgoing_initiator_owns_globally_and_retry_never_resubmits() {
    let mut h = Ownership::default();
    client(&mut h, "one", "a");
    client(&mut h, "two", "b");
    let ticket = submit(h.dial("one", "r1", "call1", "digest", 0).unwrap());
    assert_eq!(h.dial("two", "r1", "call2", "digest", 1), Err(Error::Busy));
    assert_eq!(
        h.dial("one", "r1", "call1", "digest", 1),
        Ok(Decision::Replay(ticket.clone()))
    );
    assert_eq!(
        h.dial("one", "r1", "call1", "changed", 1),
        Err(Error::Conflict)
    );
    h.result(&ticket, Some(Phase::Uncertain)).unwrap();
    assert!(h.snapshot("one").unwrap().busy);
    assert_eq!(h.result(&ticket, None), Err(Error::Stale));
    h.terminal(&authority("a"), &identity(&h)).unwrap();
    assert!(matches!(
        h.dial("one", "r1", "call1", "digest", 2),
        Ok(Decision::Replay(_))
    ));
}
#[test]
fn reject_and_answer_race_and_passive_clients_cannot_answer() {
    let mut h = Ownership::default();
    h.offer(authority("a"), "call1", 1000, 0).unwrap();
    h.connect("passive", authority("a"), "principal").unwrap();
    assert_eq!(
        h.act("passive", "r1", "call1", 1, Operation::Answer, 0),
        Err(Error::NotReady)
    );
    client(&mut h, "one", "a");
    client(&mut h, "two", "a");
    submit(
        h.act("one", "r1", "call1", 1, Operation::Reject, 0)
            .unwrap(),
    );
    assert_eq!(
        h.act("two", "r1", "call1", 1, Operation::Answer, 0),
        Err(Error::Stale)
    );
}
#[test]
fn stale_offer_cannot_answer_a_new_call_and_expired_offers_disappear() {
    let mut h = Ownership::default();
    client(&mut h, "one", "a");
    h.offer(authority("a"), "old", 2, 0).unwrap();
    h.expire(2);
    h.offer(authority("a"), "new", 1000, 3).unwrap();
    assert_eq!(
        h.act("one", "r1", "old", 1, Operation::Answer, 3),
        Err(Error::Stale)
    );
    assert_eq!(
        h.act("one", "r2", "new", 1, Operation::Answer, 1000),
        Err(Error::Stale)
    );
}
#[test]
fn owner_resume_is_principal_and_generation_bound_then_expires_once() {
    let mut h = Ownership::default();
    client(&mut h, "old", "a");
    submit(h.dial("old", "r1", "call1", "digest", 0).unwrap());
    h.disconnect("old", 1);
    h.connect("attacker", authority("a"), "another-principal")
        .unwrap();
    h.ready("attacker", true).unwrap();
    assert_eq!(h.resume("old", "attacker", 2), Err(Error::NotOwner));
    client(&mut h, "new", "a");
    h.resume("old", "new", 2).unwrap();
    assert!(h.snapshot("new").unwrap().current.unwrap().owned);
    h.disconnect("new", 3);
    assert_eq!(h.expire(5003), Some((authority("a"), identity(&h))));
    assert_eq!(h.expire(5004), None);
    assert!(h.snapshot("attacker").unwrap().busy);
}
#[test]
fn old_actor_result_does_not_mutate_new_call() {
    let mut h = Ownership::default();
    client(&mut h, "one", "a");
    let t = submit(h.dial("one", "r1", "old", "digest", 0).unwrap());
    h.result(&t, None).unwrap();
    submit(h.dial("one", "r2", "new", "digest", 1).unwrap());
    assert_eq!(h.result(&t, None), Err(Error::Stale));
    assert_eq!(
        h.terminal(&authority("b"), &identity(&h)),
        Err(Error::Stale)
    );
    assert_eq!(h.invalidate(&authority("a")), Some("new".into()));
    assert_eq!(h.snapshot("one"), Err(Error::NotConnected));
}

#[test]
fn logout_keeps_slot_until_terminal_and_resume_preserves_pending_correlation() {
    let mut h = Ownership::default();
    client(&mut h, "old", "a");
    let t = submit(h.dial("old", "r1", "call1", "digest", 0).unwrap());
    h.disconnect("old", 1);
    client(&mut h, "new", "a");
    h.resume("old", "new", 2).unwrap();
    h.result(&t, Some(Phase::Connected)).unwrap();
    assert_eq!(
        h.snapshot("new").unwrap().current.unwrap().phase,
        Phase::Connected
    );
    h.invalidate(&authority("a"));
    client(&mut h, "other", "b");
    assert_eq!(
        h.dial("other", "r1", "call2", "digest", 3),
        Err(Error::Busy)
    );
    assert_eq!(h.result(&t, None), Err(Error::Stale));
    h.terminal(&authority("a"), &identity(&h)).unwrap();
    assert!(h.dial("other", "r1", "call2", "digest", 4).is_ok());
}
#[test]
fn real_competing_threads_produce_one_submission() {
    use std::sync::{Arc, Barrier, Mutex};
    let mut h = Ownership::default();
    client(&mut h, "one", "a");
    client(&mut h, "two", "a");
    h.offer(authority("a"), "call1", 1000, 0).unwrap();
    let h = Arc::new(Mutex::new(h));
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|id| {
            let h = h.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                h.lock()
                    .unwrap()
                    .act(id, "r1", "call1", 1, Operation::Answer, 1)
            })
        })
        .collect();
    let submitted = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .filter(|r| matches!(r, Ok(Decision::Submit(_))))
        .count();
    assert_eq!(submitted, 1);
}

#[test]
fn grace_expiry_fences_late_pending_success_and_keeps_slot() {
    let mut h = Ownership::default();
    client(&mut h, "owner", "a");
    client(&mut h, "observer", "b");
    let ticket = submit(h.dial("owner", "dial", "call", "digest", 0).unwrap());
    h.disconnect("owner", 1);
    assert_eq!(h.expire(5001), Some((authority("a"), identity(&h))));
    assert_eq!(h.result(&ticket, Some(Phase::Connected)), Err(Error::Stale));
    assert_eq!(h.result(&ticket, None), Err(Error::Stale));
    assert_eq!(h.connected(&ticket), Err(Error::Stale));
    assert!(h.snapshot("observer").unwrap().busy);
    assert_eq!(
        h.dial("observer", "dial", "another", "digest", 5002),
        Err(Error::Busy)
    );
    h.terminal(&authority("a"), &identity(&h)).unwrap();
    assert!(!h.snapshot("observer").unwrap().busy);
}

#[test]
fn duplicate_disconnect_cannot_extend_grace_or_revive_invalidated_owner() {
    let mut h = Ownership::default();
    client(&mut h, "old", "a");
    submit(h.dial("old", "dial", "call", "digest", 0).unwrap());
    h.disconnect("old", 1);
    h.disconnect("old", 4999);
    assert!(h.expire(5001).is_some());

    let mut h = Ownership::default();
    client(&mut h, "old", "a");
    submit(h.dial("old", "dial", "call", "digest", 0).unwrap());
    h.invalidate(&authority("a"));
    // A delayed transport close follows the account invalidation.
    h.disconnect("old", 1);
    client(&mut h, "new", "a");
    assert_eq!(h.resume("old", "new", 2), Err(Error::NotOwner));
    assert_eq!(h.expire(5001), None);
    assert!(h.snapshot("new").unwrap().busy);
    assert!(h.snapshot("new").unwrap().current.is_none());
}

#[test]
fn resumed_socket_replays_original_requests_before_and_after_terminal() {
    let mut h = Ownership::default();
    client(&mut h, "old", "a");
    let ticket = submit(h.dial("old", "dial", "call", "digest", 0).unwrap());
    h.disconnect("old", 1);
    client(&mut h, "new", "a");
    h.resume("old", "new", 2).unwrap();
    assert_eq!(
        h.dial("new", "dial", "call", "digest", 3),
        Ok(Decision::Replay(ticket.clone()))
    );
    assert_eq!(
        h.dial("new", "dial", "call", "changed", 3),
        Err(Error::Conflict)
    );
    h.result(&ticket, Some(Phase::Connected)).unwrap();
    h.terminal(&authority("a"), &identity(&h)).unwrap();
    assert_eq!(
        h.dial("new", "dial", "call", "digest", 4),
        Ok(Decision::Replay(ticket))
    );
    assert!(!h.snapshot("new").unwrap().busy);
}

#[test]
fn cancellation_waits_for_invite_admission_and_fences_peer_connected() {
    let mut h = Ownership::default();
    client(&mut h, "owner", "a");
    let dial = submit(h.dial("owner", "dial", "call", "digest", 0).unwrap());
    assert_eq!(
        h.act("owner", "cancel", "call", 1, Operation::Cancel, 1),
        Err(Error::InvalidTransition)
    );
    assert_eq!(h.session.as_ref().unwrap().pending, Some(dial.clone()));
    h.result(&dial, Some(Phase::Dialing)).unwrap();
    let cancel = submit(
        h.act("owner", "cancel", "call", 2, Operation::Cancel, 2)
            .unwrap(),
    );
    assert_eq!(h.connected(&dial), Err(Error::Stale));
    assert_eq!(h.result(&dial, Some(Phase::Connected)), Err(Error::Stale));
    h.result(&cancel, None).unwrap();
    assert!(!h.snapshot("owner").unwrap().busy);
}

#[test]
fn peer_connection_is_separate_from_invite_ack_and_attempt_bound() {
    let mut h = Ownership::default();
    client(&mut h, "owner", "a");
    let dial = submit(h.dial("owner", "first", "call", "digest", 0).unwrap());
    assert_eq!(h.connected(&dial), Err(Error::Stale));
    h.result(&dial, Some(Phase::Dialing)).unwrap();
    h.connected(&dial).unwrap();
    assert_eq!(
        h.snapshot("owner").unwrap().current.unwrap().phase,
        Phase::Connected
    );
    h.terminal(&authority("a"), &identity(&h)).unwrap();
    let next = submit(h.dial("owner", "second", "call", "digest", 1).unwrap());
    assert_ne!(dial.attempt, next.attempt);
    assert_eq!(h.result(&dial, None), Err(Error::Stale));
    assert_eq!(h.connected(&dial), Err(Error::Stale));
    assert!(h.snapshot("owner").unwrap().busy);
}

#[test]
fn checked_resume_exhaustion_does_not_partially_transfer_owner() {
    let mut h = Ownership::default();
    client(&mut h, "old", "a");
    submit(h.dial("old", "dial", "call", "digest", 0).unwrap());
    h.disconnect("old", 1);
    client(&mut h, "new", "a");
    let original_logical_id = h.clients.get("new").unwrap().logical_id;
    h.session.as_mut().unwrap().revision = u64::MAX;
    assert_eq!(h.resume("old", "new", 2), Err(Error::Capacity));
    let session = h.session.as_ref().unwrap();
    assert_eq!(session.owner.as_deref(), Some("old"));
    assert_eq!(session.resume_until, Some(5001));
    assert_eq!(
        h.clients.get("new").unwrap().logical_id,
        original_logical_id
    );
}

#[test]
fn checked_attempt_exhaustion_never_allocates_a_session() {
    let mut h = Ownership::default();
    client(&mut h, "owner", "a");
    h.next_attempt = u64::MAX;
    assert_eq!(
        h.dial("owner", "dial", "call", "digest", 0),
        Err(Error::Capacity)
    );
    assert_eq!(
        h.offer(authority("a"), "incoming", 1000, 0),
        Err(Error::Capacity)
    );
    assert!(h.session.is_none());
    assert!(h.requests.is_empty());
}

#[test]
fn reused_public_call_id_cannot_admit_a_delayed_terminal() {
    let mut h = Ownership::default();
    client(&mut h, "owner", "a");
    let old = h.offer(authority("a"), "same-id", 1000, 0).unwrap();
    h.terminal(&authority("a"), &old).unwrap();
    let current = h.offer(authority("a"), "same-id", 1000, 1).unwrap();
    assert_ne!(old, current);
    assert_eq!(h.terminal(&authority("a"), &old), Err(Error::Stale));
    assert!(h.snapshot("owner").unwrap().busy);
    h.terminal(&authority("a"), &current).unwrap();
}

#[test]
fn reused_transport_label_does_not_replay_another_principals_receipt() {
    let mut h = Ownership::default();
    h.connect("socket", authority("a"), "original-principal")
        .unwrap();
    h.ready("socket", true).unwrap();
    let old = submit(
        h.dial("socket", "dial", "same-call-id", "digest", 0)
            .unwrap(),
    );
    h.result(&old, None).unwrap();
    h.disconnect("socket", 1);
    h.connect("socket", authority("a"), "another-principal")
        .unwrap();
    h.ready("socket", true).unwrap();
    let next = submit(
        h.dial("socket", "dial", "same-call-id", "digest", 2)
            .unwrap(),
    );
    assert_ne!(old.attempt, next.attempt);
}

#[test]
fn reused_label_cannot_own_or_control_the_disconnected_owners_call() {
    let mut h = Ownership::default();
    h.connect("socket", authority("a"), "original-principal")
        .unwrap();
    h.ready("socket", true).unwrap();
    let ticket = submit(h.dial("socket", "dial", "call", "digest", 0).unwrap());
    h.result(&ticket, Some(Phase::Connected)).unwrap();
    h.disconnect("socket", 1);
    h.connect("socket", authority("a"), "another-principal")
        .unwrap();
    h.ready("socket", true).unwrap();
    assert!(h.snapshot("socket").unwrap().current.is_none());
    assert_eq!(
        h.act("socket", "hangup", "call", 2, Operation::Hangup, 2),
        Err(Error::NotOwner)
    );
    assert_eq!(h.resume("socket", "socket", 2), Err(Error::NotOwner));
    h.disconnect("socket", 4999);
    assert!(h.expire(5001).is_some());
}

#[test]
fn checked_client_identity_exhaustion_leaves_connections_unchanged() {
    let mut h = Ownership {
        next_client: u64::MAX,
        ..Ownership::default()
    };
    assert_eq!(
        h.connect("socket", authority("a"), "principal"),
        Err(Error::Capacity)
    );
    assert!(h.clients.is_empty());
    assert!(h.requests.is_empty());
    assert!(h.session.is_none());
}

#[test]
fn uncertain_callback_is_attempt_bound_and_keeps_the_slot() {
    let mut h = Ownership::default();
    client(&mut h, "owner", "a");
    let old = h.offer(authority("a"), "same", 1000, 0).unwrap();
    h.terminal(&authority("a"), &old).unwrap();
    let current = h.offer(authority("a"), "same", 1000, 1).unwrap();
    assert_eq!(h.uncertain(&authority("a"), &old), Err(Error::Stale));
    h.session.as_mut().unwrap().revision = u64::MAX;
    assert_eq!(h.uncertain(&authority("a"), &current), Err(Error::Capacity));
    assert_eq!(h.session.as_ref().unwrap().phase, Phase::Ringing);
    h.session.as_mut().unwrap().revision = 1;
    h.uncertain(&authority("a"), &current).unwrap();
    assert_eq!(h.session.as_ref().unwrap().phase, Phase::Uncertain);
    assert!(h.session.as_ref().unwrap().pending.is_none());
    assert!(h.snapshot("owner").unwrap().busy);
}
