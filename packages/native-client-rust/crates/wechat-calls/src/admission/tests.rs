use super::*;
fn authority(account: &str) -> AccountAuthority {
    AccountAuthority {
        account_id: account.into(),
        generation: "generation1".into(),
    }
}
fn submit(d: Decision) -> Ticket {
    match d {
        Decision::Submit(t) => t,
        _ => panic!("must be unique"),
    }
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
    assert_eq!(h.result(&ticket, Some(Phase::Connected)), Err(Error::Stale));
    assert_eq!(
        h.act("socket", "hangup", "call", 2, Operation::Hangup, 2),
        Err(Error::NotOwner)
    );
    assert!(h.expire(2).is_some());
    h.disconnect("socket", 4999);
    assert!(h.expire(5001).is_none());

    let old = CallIdentity {
        attempt: ticket.attempt,
        call_id: "call".into(),
    };
    h.terminal(&authority("a"), &old).unwrap();
    h.connect("socket", authority("a"), "later-principal")
        .unwrap();
    h.ready("socket", true).unwrap();
    let next = submit(h.dial("socket", "dial", "call", "digest", 5002).unwrap());
    let current = CallIdentity {
        attempt: next.attempt,
        call_id: "call".into(),
    };
    assert_ne!(old, current);
    assert_eq!(h.terminal(&authority("a"), &old), Err(Error::Stale));
    assert_eq!(h.uncertain(&authority("a"), &old), Err(Error::Stale));
    h.uncertain(&authority("a"), &current).unwrap();
    assert_eq!(h.session.as_ref().unwrap().phase, Phase::Uncertain);
    assert!(h.snapshot("socket").unwrap().busy);
    h.terminal(&authority("a"), &current).unwrap();
}

#[test]
fn uncertain_start_replays_its_ticket_and_reserves_the_account_until_cleanup() {
    let mut admission = Ownership::default();
    for controller in ["first", "second"] {
        admission
            .connect(controller, authority("a"), "caller")
            .unwrap();
        admission.ready(controller, true).unwrap();
    }
    let ticket = submit(
        admission
            .dial("first", "request", "call", "digest", 0)
            .unwrap(),
    );
    admission.result(&ticket, Some(Phase::Uncertain)).unwrap();
    assert_eq!(
        admission.dial("first", "request", "call", "digest", 1),
        Ok(Decision::Replay(ticket.clone()))
    );
    assert_eq!(
        admission.dial("first", "request", "call", "changed-digest", 1),
        Err(Error::Conflict)
    );
    assert_eq!(
        admission.dial("second", "different-request", "different-call", "digest", 1),
        Err(Error::Busy)
    );
    admission
        .terminal(&authority("a"), &ticket.identity())
        .unwrap();
    let next = submit(
        admission
            .dial("second", "different-request", "different-call", "digest", 2)
            .unwrap(),
    );
    assert_eq!(admission.result(&ticket, None), Err(Error::Stale));
    assert!(admission.pending_ticket_matches(&next));
}

#[test]
fn replaced_account_authority_cannot_adopt_a_late_start_result() {
    let mut admission = Ownership::default();
    let original = authority("a");
    admission
        .connect("old", original.clone(), "caller")
        .unwrap();
    admission.ready("old", true).unwrap();
    let old = submit(
        admission
            .dial("old", "request", "call", "digest", 0)
            .unwrap(),
    );
    assert_eq!(admission.invalidate(&original), Some("call".into()));
    assert_eq!(
        admission.result(&old, Some(Phase::Connected)),
        Err(Error::Stale)
    );
    let replacement = AccountAuthority {
        generation: "generation2".into(),
        ..original.clone()
    };
    admission
        .connect("new", replacement.clone(), "caller")
        .unwrap();
    admission.ready("new", true).unwrap();
    assert!(admission.snapshot("new").unwrap().current.is_none());
    assert_eq!(
        admission.terminal(&replacement, &old.identity()),
        Err(Error::Stale)
    );
    assert_eq!(
        admission.dial("new", "next", "next-call", "digest", 1),
        Err(Error::Busy)
    );
    // Retiring local work still uses the original attempt. A new login is not
    // evidence that an unresolved remote start never reached the server.
    admission.terminal(&original, &old.identity()).unwrap();
    let next = submit(
        admission
            .dial("new", "next", "next-call", "digest", 2)
            .unwrap(),
    );
    assert_eq!(next.authority(), &replacement);
}
