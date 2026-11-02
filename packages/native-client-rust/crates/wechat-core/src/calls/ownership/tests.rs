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
    assert_eq!(
        h.act("socket", "hangup", "call", 2, Operation::Hangup, 2),
        Err(Error::NotOwner)
    );
    assert_eq!(h.resume("socket", "socket", 2), Err(Error::NotOwner));
    h.disconnect("socket", 4999);
    assert!(h.expire(5001).is_some());

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
