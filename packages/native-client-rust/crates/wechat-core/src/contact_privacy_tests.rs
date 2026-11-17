use super::*;
use wechat_profile::{ContactProjection, SqlValue};
use wechat_protocol::protobuf as p;

fn set_fields(core: &Core, username: &str, fields: &[(&str, &str)]) {
    let (location, keys) = core.profile_location_keys("one").unwrap();
    wechat_profile::apply_contact_patch(
        &location,
        &keys,
        &ContactProjection {
            username: username.into(),
            fields: fields
                .iter()
                .map(|(name, value)| ((*name).into(), SqlValue::Integer((*value).into())))
                .collect(),
        },
        &core.write_options("one", "privacy-test").unwrap(),
    )
    .unwrap();
}

fn update(core: &Core, privacy: ContactPrivacySettings) -> ContactMutation {
    ContactMutation::UpdatePrivacy {
        username: "peer".into(),
        privacy,
        expected_version: core.contact_privacy("one", "peer").unwrap()["version"]
            .as_str()
            .unwrap()
            .into(),
    }
}

fn successful_reply(rpc: &ContactRpc) -> Vec<u8> {
    if rpc.cgi != 3990 {
        return p::bytes(1, &p::uint(1, 0));
    }
    let fields = p::decode(&rpc.body).unwrap();
    let items: Vec<_> = fields.iter().filter(|field| field.number == 3).collect();
    let mut response = vec![p::bytes(1, &p::uint(1, 0)), p::uint(2, items.len() as u64)];
    for item in items {
        let p::Value::Bytes(raw) = &item.value else {
            panic!("invalid fixture item")
        };
        response.push(p::bytes(
            3,
            &p::message(&[p::uint(1, 0), p::bytes(2, raw)]).unwrap(),
        ));
    }
    p::message(&response).unwrap()
}

fn receive_and_finish(core: &Core, op: &Operation) {
    receive_reply_and_finish(core, op, None)
}

fn receive_reply_and_finish(core: &Core, op: &Operation, reply: Option<Vec<u8>>) {
    let record = core.store.contact_mutation("one", &op.id).unwrap();
    let (attempt, _) = core.load_contact_attempt(&record).unwrap();
    let cipher = core.store.runtime("one", "account-state").unwrap().unwrap();
    assert!(core
        .store
        .begin_contact_step(
            "one",
            &op.id,
            0,
            &cipher,
            attempt.epoch.as_deref(),
            &attempt.owner
        )
        .unwrap());
    let mut receipts = Receipts {
        reply: reply.or_else(|| attempt.rpc.as_ref().map(successful_reply)),
        time: Some(wechat_store::unix_seconds()),
        ..Default::default()
    };
    core.store
        .receive_contact_step(
            "one",
            &op.id,
            0,
            &core
                .vault
                .seal(
                    "one",
                    &purpose(&op.id, "receipts"),
                    &serde_json::to_vec(&receipts).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
    core.finish_contact("one", &op.id, &attempt, &mut receipts)
        .unwrap();
}

#[test]
fn contact_privacy_optional_fields_preserve_existing_idempotency_inputs() {
    for original in [
        r#"{"action":"send_request","recipientRef":"handle","message":"hello"}"#,
        r#"{"action":"accept_request","id":"request","expectedVersion":"version"}"#,
    ] {
        let action: ContactMutation = serde_json::from_str(original).unwrap();
        assert_eq!(serde_json::to_string(&action).unwrap(), original);
        assert_eq!(
            digest(&serde_json::to_vec(&action).unwrap()),
            digest(original.as_bytes())
        );
    }
    let action: ContactMutation = serde_json::from_value(json!({"action":"send_request","recipientRef":"handle","message":"hello","privacy":{"chatsOnly":false,"hideMyPosts":true,"hideTheirPosts":false}})).unwrap();
    assert_ne!(
        serde_json::to_string(&action).unwrap(),
        r#"{"action":"send_request","recipientRef":"handle","message":"hello"}"#
    );
}

#[test]
fn contact_privacy_version_binds_account_peer_full_flags_and_membership() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let first = core.contact_privacy("one", "peer").unwrap();
    assert_eq!(first["privacy"], json!(ContactPrivacySettings::default()));
    let peer = core
        .profile_reader_owned("one")
        .unwrap()
        .contact("peer")
        .unwrap();
    let one = canonical_privacy("one", "peer", &peer).unwrap();
    assert_ne!(
        one.version,
        canonical_privacy("two", "peer", &peer).unwrap().version
    );
    let other = core
        .profile_reader_owned("one")
        .unwrap()
        .contact("other")
        .unwrap();
    assert_ne!(
        one.version,
        canonical_privacy("one", "other", &other).unwrap().version
    );
    set_fields(&core, "peer", &[("flag", "513")]);
    let unrelated_change = core.contact_privacy("one", "peer").unwrap();
    assert_eq!(first["privacy"], unrelated_change["privacy"]);
    assert_ne!(first["version"], unrelated_change["version"]);
    assert_eq!(
        core.current_contact_privacy("one", "peer", Some(&one.version))
            .unwrap_err()
            .to_string(),
        "stale_contact_privacy"
    );
    set_fields(&core, "peer", &[("delete_flag", "1")]);
    let removed = core
        .profile_reader_owned("one")
        .unwrap()
        .contact("peer")
        .unwrap();
    assert_ne!(
        canonical_privacy("one", "peer", &removed).unwrap().version,
        unrelated_change["version"].as_str().unwrap()
    );
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap_err().to_string(),
        "ordinary_contact_required"
    );
}

#[test]
fn contact_privacy_reads_only_supported_saved_ordinary_contacts() {
    let (_temp, core) = super::super::groups::tests::fixture();
    set_fields(&core, "peer", &[("flag", "8454401")]);
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!({"chatsOnly":true,"hideMyPosts":true,"hideTheirPosts":true})
    );
    for fields in [
        vec![("local_type", "5")],
        vec![("local_type", "1"), ("verify_flag", "8")],
        vec![("verify_flag", "0"), ("flag", "0")],
    ] {
        set_fields(&core, "peer", &fields);
        assert_eq!(
            core.contact_privacy("one", "peer").unwrap_err().to_string(),
            "ordinary_contact_required"
        );
    }
    for username in [
        "group@chatroom",
        "work@openim",
        "filehelper",
        "synthetic_self",
    ] {
        set_fields(
            &core,
            username,
            &[("local_type", "1"), ("flag", "1"), ("delete_flag", "0")],
        );
        assert_eq!(
            core.contact_privacy("one", username)
                .unwrap_err()
                .to_string(),
            "ordinary_contact_required"
        );
    }
}

#[tokio::test]
async fn contact_privacy_rechecks_stale_versions_before_any_submission() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let desired = ContactPrivacySettings {
        chats_only: true,
        ..Default::default()
    };
    let action = update(&core, desired);
    let (op, _) = core
        .admit_contact("one", action.clone(), "privacy-stale")
        .unwrap();
    set_fields(&core, "peer", &[("flag", "257")]);
    assert_eq!(
        core.admit_contact("one", action, "privacy-other-key")
            .unwrap_err()
            .to_string(),
        "stale_contact_privacy"
    );
    let guard = core.lock_account("one").await.unwrap();
    let mut permit = crate::account_network::AccountNetworkPermit {
        guard: Some(guard),
        runtime: tokio::runtime::Handle::current(),
    };
    assert_eq!(
        core.execute_contact("one", &op.id, &mut permit)
            .unwrap_err()
            .to_string(),
        "stale_contact_privacy"
    );
    let record = core.store.contact_mutation("one", &op.id).unwrap();
    assert_eq!(record.phase, "queued");
    assert!(core
        .load_contact_attempt(&record)
        .unwrap()
        .1
        .reply
        .is_none());
    core.hold_contact("one", &op.id, Some("stale_contact_privacy"))
        .unwrap();
    let failed = core.store.contact_mutation("one", &op.id).unwrap();
    assert_eq!(failed.operation.state, "failed");
    assert_eq!(failed.operation.result["code"], "stale_contact_privacy");
    assert_eq!(failed.operation.result["remoteOutcome"], "not_attempted");
}

#[test]
fn contact_privacy_noop_has_no_rpc_and_preserves_canonical_flags() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let action = update(&core, ContactPrivacySettings::default());
    let (op, _) = core
        .admit_contact("one", action.clone(), "privacy-noop")
        .unwrap();
    assert!(core
        .load_contact_attempt(&core.store.contact_mutation("one", &op.id).unwrap())
        .unwrap()
        .0
        .rpc
        .is_none());
    receive_and_finish(&core, &op);
    let done = core.store.contact_mutation("one", &op.id).unwrap();
    assert_eq!(done.operation.result["remoteOutcome"], "not_applicable");
    assert_eq!(done.operation.result["profileProjection"], "complete");
    assert_eq!(done.operation.result["changed"], false);
    assert_eq!(
        core.profile_reader_owned("one")
            .unwrap()
            .contact("peer")
            .unwrap()
            .raw_fields["flag"],
        SqlValue::Integer("1".into())
    );
    assert!(!core.admit_contact("one", action, "privacy-noop").unwrap().1);
}

#[test]
fn contact_privacy_ack_waits_for_matching_canonical_sync_and_recovery_never_replays() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let desired = ContactPrivacySettings {
        chats_only: true,
        ..Default::default()
    };
    let (op, _) = core
        .admit_contact("one", update(&core, desired), "privacy-sync")
        .unwrap();
    receive_and_finish(&core, &op);
    let pending = core.store.contact_mutation("one", &op.id).unwrap();
    assert_eq!(pending.operation.result["remoteOutcome"], "accepted");
    assert_eq!(
        pending.operation.result["profileProjection"],
        "awaiting_contact_sync"
    );
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap_err().to_string(),
        "contact_privacy_pending"
    );
    assert_eq!(
        core.profile_reader_owned("one")
            .unwrap()
            .contact("peer")
            .unwrap()
            .raw_fields["flag"],
        SqlValue::Integer("1".into())
    );
    core.repair_contact_mutations("one").unwrap();
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "awaiting_contact_sync"
    );
    set_fields(&core, "other", &[("flag", "8388609")]);
    core.settle_contact_relationship("one", "other", true, "other-sync")
        .unwrap();
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "awaiting_contact_sync"
    );
    set_fields(&core, "peer", &[("flag", "257")]);
    core.settle_contact_relationship("one", "peer", true, "wrong-privacy")
        .unwrap();
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "awaiting_contact_sync"
    );
    set_fields(&core, "peer", &[("flag", "8388609")]);
    core.settle_contact_relationship("one", "peer", true, "privacy-sync")
        .unwrap();
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "complete"
    );
    core.repair_contact_mutations("one").unwrap();
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!(desired)
    );
}

#[test]
fn accepting_with_privacy_requires_canonical_privacy_as_well_as_membership() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let privacy = ContactPrivacySettings {
        hide_my_posts: true,
        ..Default::default()
    };
    let (template, _) = core
        .admit_contact("one", update(&core, privacy), "accept-template")
        .unwrap();
    let (mut attempt, _) = core
        .load_contact_attempt(&core.store.contact_mutation("one", &template.id).unwrap())
        .unwrap();
    let target = ContactRequestTarget {
        username: "peer".into(),
        ticket: "synthetic-ticket".into(),
        scene: 3,
    };
    attempt.action = ContactMutation::AcceptRequest {
        id: request_id("peer"),
        expected_version: "synthetic-request-version".into(),
        privacy: Some(privacy),
    };
    attempt.target = Some(target.clone());
    attempt.rpc = Some(
        wechat_protocol::accept_contact_request_rpc(
            &core.account_state("one").unwrap(),
            &AcceptContactRequest {
                target,
                privacy: Some(privacy),
            },
        )
        .unwrap(),
    );
    let op = Operation {
        id: uuid::Uuid::new_v4().to_string(),
        result: json!({"action":"accept_request","profileProjection":"pending","remoteOutcome":"not_attempted"}),
        ..template
    };
    core.store
        .create_contact_mutation(
            &op,
            "accept-with-privacy",
            &digest(b"accept-privacy"),
            &core
                .vault
                .seal(
                    "one",
                    &purpose(&op.id, "request"),
                    &serde_json::to_vec(&attempt).unwrap(),
                )
                .unwrap(),
            &core
                .vault
                .seal(
                    "one",
                    &purpose(&op.id, "receipts"),
                    &serde_json::to_vec(&Receipts::default()).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
    receive_and_finish(&core, &op);
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "awaiting_contact_sync"
    );
    set_fields(&core, "peer", &[("flag", "257")]);
    core.settle_contact_relationship("one", "peer", true, "accept-privacy-sync")
        .unwrap();
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "complete"
    );
}

fn privacy_record(mask: u32, flags: u32, controlled: bool) -> wechat_protocol::wire::ContactRecord {
    let mut fields = vec![
        p::bytes(1, &p::bytes(1, b"peer")),
        p::uint(7, u64::from(mask)),
        p::uint(8, u64::from(flags)),
    ];
    if controlled {
        let info = p::message(&[
            p::uint(1, 1),
            p::uint(2, 2),
            p::uint(3, 1),
            p::bytes(4, &p::uint(1, 8)),
        ])
        .unwrap();
        fields.push(p::bytes(
            89,
            &p::message(&[p::uint(1, 1), p::bytes(2, &info)]).unwrap(),
        ));
    }
    wechat_protocol::wire::decode_contact_record(&p::message(&fields).unwrap()).unwrap()
}

#[test]
fn contact_privacy_unresolved_receipts_block_reads_writes_and_stale_repair() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let desired = ContactPrivacySettings {
        hide_my_posts: true,
        ..Default::default()
    };
    let action = update(&core, desired);
    let (op, _) = core
        .admit_contact("one", action.clone(), "privacy-controlled")
        .unwrap();
    receive_and_finish(&core, &op);
    let controlled = privacy_record(u32::MAX, 257, true);
    core.invalidate_contact_privacy("one", &controlled, "privacy-controlled-source")
        .unwrap();
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap_err().to_string(),
        "contact_privacy_unresolved"
    );
    assert_eq!(
        core.admit_contact("one", action, "privacy-while-unresolved")
            .unwrap_err()
            .to_string(),
        "contact_privacy_unresolved"
    );
    let withheld = core.upstream.contact_patch(&controlled).unwrap();
    assert!(!withheld.fields.contains_key("flag"));
    core.settle_contact_privacy_projection(
        "one",
        &controlled,
        &withheld,
        "privacy-controlled-source",
    )
    .unwrap();
    assert!(!core.contact_privacy_resolved("one", "peer").unwrap());
    let full = privacy_record(u32::MAX, 257, false);
    core.invalidate_contact_privacy("one", &full, "privacy-exact-source")
        .unwrap();
    let patch = core.upstream.contact_patch(&full).unwrap();
    core.settle_contact_privacy_projection("one", &full, &patch, "privacy-exact-source")
        .unwrap();
    assert!(!core.contact_privacy_resolved("one", "peer").unwrap());
    set_fields(&core, "peer", &[("flag", "257")]);
    core.settle_contact_privacy_projection("one", &full, &patch, "privacy-exact-source")
        .unwrap();
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!(desired)
    );
    assert_eq!(
        core.store
            .contact_mutation("one", &op.id)
            .unwrap()
            .operation
            .result["profileProjection"],
        "complete"
    );
    let partial = privacy_record(0x800000, 0x800000, false);
    core.invalidate_contact_privacy("one", &partial, "privacy-newer-partial")
        .unwrap();
    core.invalidate_contact_privacy("one", &full, "privacy-exact-source")
        .unwrap();
    core.settle_contact_privacy_projection("one", &full, &patch, "privacy-exact-source")
        .unwrap();
    assert!(!core.contact_privacy_resolved("one", "peer").unwrap());
}

#[test]
fn contact_privacy_unrelated_partial_masks_do_not_invalidate_privacy() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let mute = privacy_record(0x200, 0x200, false);
    core.invalidate_contact_privacy("one", &mute, "mute-only")
        .unwrap();
    assert!(core.contact_privacy_resolved("one", "peer").unwrap());
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!(ContactPrivacySettings::default())
    );
}

#[test]
fn contact_privacy_read_uses_profile_identity_without_network_authorization() {
    let (_temp, core) = super::super::groups::tests::fixture();
    core.store.put_runtime("one", "logged-out", b"1").unwrap();
    assert!(core.account_state("one").is_err());
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!(ContactPrivacySettings::default())
    );
}

#[test]
fn contact_privacy_queued_and_accepted_intents_block_new_keys_but_not_key_replay() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let privacy = ContactPrivacySettings {
        chats_only: true,
        ..Default::default()
    };
    let action = update(&core, privacy);
    let (first, _) = core
        .admit_contact("one", action.clone(), "privacy-first")
        .unwrap();
    assert_eq!(
        core.admit_contact("one", action.clone(), "privacy-queued")
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
    core.ensure_no_active_contact_privacy("one", "peer", Some(&first.id))
        .unwrap();
    assert_eq!(
        core.ensure_no_active_contact_privacy("one", "peer", None)
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
    receive_and_finish(&core, &first);
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap_err().to_string(),
        "contact_privacy_pending"
    );
    let (replayed, created) = core
        .admit_contact("one", action.clone(), "privacy-first")
        .unwrap();
    assert!(!created);
    assert_eq!(replayed.id, first.id);
    assert_eq!(
        core.admit_contact("one", action, "privacy-new-key")
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
}

#[test]
fn contact_privacy_unknown_outcome_holds_other_keys_without_hiding_canonical_reads_or_replaying() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let privacy = ContactPrivacySettings {
        chats_only: true,
        ..Default::default()
    };
    let action = update(&core, privacy);
    let (first, _) = core
        .admit_contact("one", action.clone(), "privacy-unknown-original")
        .unwrap();
    let record = core.store.contact_mutation("one", &first.id).unwrap();
    let (attempt, _) = core.load_contact_attempt(&record).unwrap();
    let cipher = core.store.runtime("one", "account-state").unwrap().unwrap();
    assert!(core
        .store
        .begin_contact_step(
            "one",
            &first.id,
            0,
            &cipher,
            attempt.epoch.as_deref(),
            &attempt.owner
        )
        .unwrap());
    core.hold_contact("one", &first.id, None).unwrap();
    let (replayed, created) = core
        .admit_contact("one", action.clone(), "privacy-unknown-original")
        .unwrap();
    assert!(!created);
    assert_eq!(replayed.id, first.id);
    assert_eq!(replayed.state, "needs_review");
    assert_eq!(
        core.admit_contact("one", action, "privacy-unknown-other-key")
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!(ContactPrivacySettings::default())
    );
    let other = ContactMutation::UpdatePrivacy {
        username: "other".into(),
        privacy,
        expected_version: core.contact_privacy("one", "other").unwrap()["version"]
            .as_str()
            .unwrap()
            .into(),
    };
    assert!(
        core.admit_contact("one", other, "privacy-other-peer")
            .unwrap()
            .1
    );
    // A later matching canonical row proves its current value, not this unknown RPC's outcome.
    set_fields(&core, "peer", &[("flag", "8388609")]);
    core.settle_contact_relationship("one", "peer", true, "unknown-matching-row")
        .unwrap();
    core.repair_contact_mutations("one").unwrap();
    let held = core.store.contact_mutation("one", &first.id).unwrap();
    assert_eq!(held.phase, "needs_review");
    assert_eq!(held.operation.state, "needs_review");
    assert_eq!(held.operation.result["remoteOutcome"], "unknown");
    assert!(core.load_contact_attempt(&held).unwrap().1.reply.is_none());
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!(privacy)
    );
    assert_eq!(
        core.admit_contact("one", update(&core, privacy), "privacy-unknown-after-sync")
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
}

#[test]
fn partial_privacy_reply_reconciles_actual_snapshot_without_retry_or_forcing_desired() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let before = privacy_record(u32::MAX, 1, false);
    core.invalidate_contact_privacy("one", &before, "before-partial")
        .unwrap();
    let before_patch = core.upstream.contact_patch(&before).unwrap();
    core.settle_contact_privacy_projection("one", &before, &before_patch, "before-partial")
        .unwrap();
    let desired = ContactPrivacySettings {
        chats_only: true,
        hide_my_posts: true,
        ..Default::default()
    };
    let action = update(&core, desired);
    let (op, _) = core
        .admit_contact("one", action.clone(), "privacy-partial")
        .unwrap();
    let record = core.store.contact_mutation("one", &op.id).unwrap();
    let (attempt, _) = core.load_contact_attempt(&record).unwrap();
    let request = p::decode(&attempt.rpc.as_ref().unwrap().body).unwrap();
    let mut response = vec![p::bytes(1, &p::uint(1, 0)), p::uint(2, 2)];
    for (item, code) in request
        .iter()
        .filter(|field| field.number == 3)
        .zip([0i32, -7])
    {
        let p::Value::Bytes(raw) = &item.value else {
            panic!("invalid fixture item")
        };
        response.push(p::bytes(
            3,
            &p::message(&[p::uint(1, code as i64 as u64), p::bytes(2, raw)]).unwrap(),
        ));
    }
    receive_reply_and_finish(&core, &op, Some(p::message(&response).unwrap()));
    let failed = core.store.contact_mutation("one", &op.id).unwrap();
    assert_eq!(failed.operation.state, "failed");
    assert_eq!(failed.operation.result["remoteOutcome"], "partial");
    assert_eq!(failed.operation.result["itemCodes"], json!([0, -7]));
    assert_eq!(
        failed.operation.result["profileProjection"],
        "awaiting_contact_sync"
    );
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap_err().to_string(),
        "contact_privacy_unresolved"
    );
    core.invalidate_contact_privacy("one", &before, "before-partial")
        .unwrap();
    core.settle_contact_privacy_projection("one", &before, &before_patch, "before-partial")
        .unwrap();
    assert!(!core.contact_privacy_resolved("one", "peer").unwrap());
    let actual = privacy_record(u32::MAX, 8388609, false);
    core.invalidate_contact_privacy("one", &actual, "after-partial")
        .unwrap();
    let patch = core.upstream.contact_patch(&actual).unwrap();
    set_fields(&core, "peer", &[("flag", "8388609")]);
    core.settle_contact_privacy_projection("one", &actual, &patch, "after-partial")
        .unwrap();
    let settled = core.store.contact_mutation("one", &op.id).unwrap();
    assert_eq!(settled.phase, "failed");
    assert_eq!(settled.operation.state, "failed");
    assert_eq!(settled.operation.result["partial"], true);
    assert_eq!(settled.operation.result["remoteOutcome"], "partial");
    assert_eq!(settled.operation.result["profileProjection"], "complete");
    assert_eq!(
        core.contact_privacy("one", "peer").unwrap()["privacy"],
        json!({"chatsOnly":true,"hideMyPosts":false,"hideTheirPosts":false})
    );
    core.repair_contact_mutations("one").unwrap();
    assert!(
        !core
            .admit_contact("one", action, "privacy-partial")
            .unwrap()
            .1
    );
    assert_eq!(
        core.profile_reader_owned("one")
            .unwrap()
            .contact("peer")
            .unwrap()
            .raw_fields["flag"],
        SqlValue::Integer("8388609".into())
    );
}

#[tokio::test]
async fn contact_privacy_overflow_holds_admission_and_submission_but_replays_the_original_key() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let desired = ContactPrivacySettings {
        chats_only: true,
        ..Default::default()
    };
    set_fields(
        &core,
        "capacity-peer",
        &[("local_type", "1"), ("flag", "1"), ("delete_flag", "0")],
    );
    let fresh_action = ContactMutation::UpdatePrivacy {
        username: "capacity-peer".into(),
        privacy: desired,
        expected_version: core.contact_privacy("one", "capacity-peer").unwrap()["version"]
            .as_str()
            .unwrap()
            .into(),
    };
    let action = update(&core, desired);
    let (first, _) = core
        .admit_contact("one", action.clone(), "privacy-capacity-original")
        .unwrap();
    let original = core.store.contact_mutation("one", &first.id).unwrap();
    let (mut attempt, _) = core.load_contact_attempt(&original).unwrap();
    let other_before = ContactPrivacySettings::default();
    attempt.action = ContactMutation::UpdatePrivacy {
        username: "other".into(),
        privacy: desired,
        expected_version: core.contact_privacy("one", "other").unwrap()["version"]
            .as_str()
            .unwrap()
            .into(),
    };
    attempt.rpc = Some(
        wechat_protocol::contact_privacy_request(
            &core.account_state("one").unwrap(),
            "other",
            &other_before,
            &desired,
        )
        .unwrap(),
    );
    let request = serde_json::to_vec(&attempt).unwrap();
    let receipts = serde_json::to_vec(&Receipts {
        reply: Some(successful_reply(attempt.rpc.as_ref().unwrap())),
        ..Default::default()
    })
    .unwrap();
    // Neither individual batch is full, but their combined guard candidates exceed 2000.
    for index in 0..2001 {
        let id = format!("privacy-capacity-{index:04}");
        let mut operation = Operation {
            id: id.clone(),
            account_id: "one".into(),
            kind: "contact_mutation".into(),
            state: "queued".into(),
            result: json!({"action":"update_privacy","profileProjection":"awaiting_contact_sync"}),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        core.store
            .create_contact_mutation(
                &operation,
                &id,
                &digest(&request),
                &core
                    .vault
                    .seal("one", &purpose(&id, "request"), &request)
                    .unwrap(),
                &core
                    .vault
                    .seal("one", &purpose(&id, "receipts"), &receipts)
                    .unwrap(),
            )
            .unwrap();
        if index < 1001 {
            for (phase, state) in [
                ("needs_review", "needs_review"),
                ("projecting", "running"),
                ("succeeded", "succeeded"),
            ] {
                operation.state = state.into();
                core.store
                    .advance_contact_mutation(&operation, phase, 0)
                    .unwrap();
            }
        }
    }
    assert_eq!(
        core.store
            .active_contact_mutations("one", Some(&first.id))
            .unwrap()
            .len(),
        1000
    );
    assert_eq!(
        core.store
            .pending_contact_relationships("one")
            .unwrap()
            .len(),
        1001
    );
    assert_eq!(
        core.admit_contact("one", fresh_action, "privacy-capacity-new-key")
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
    let (replayed, created) = core
        .admit_contact("one", action, "privacy-capacity-original")
        .unwrap();
    assert!(!created);
    assert_eq!(replayed.id, first.id);
    let guard = core.lock_account("one").await.unwrap();
    let mut permit = crate::account_network::AccountNetworkPermit {
        guard: Some(guard),
        runtime: tokio::runtime::Handle::current(),
    };
    assert_eq!(
        core.execute_contact("one", &first.id, &mut permit)
            .unwrap_err()
            .to_string(),
        "contact_privacy_pending"
    );
    let held = core.store.contact_mutation("one", &first.id).unwrap();
    assert_eq!(held.phase, "queued");
    assert!(core.load_contact_attempt(&held).unwrap().1.reply.is_none());
    assert_eq!(
        core.profile_reader_owned("one")
            .unwrap()
            .contact("peer")
            .unwrap()
            .raw_fields["flag"],
        SqlValue::Integer("1".into())
    );
}

#[test]
fn contact_relation_distinguishes_cached_unknown_and_removed_people() {
    let (_temp, core) = super::super::groups::tests::fixture();
    let mut contact = core
        .profile_reader_owned("one")
        .unwrap()
        .contact("peer")
        .unwrap();
    assert_eq!(contact_relation(&contact), "friend");
    contact
        .raw_fields
        .insert("flag".into(), SqlValue::Integer("0".into()));
    assert_eq!(contact_relation(&contact), "non_contact");
    contact.raw_fields.insert("flag".into(), SqlValue::Null);
    assert_eq!(contact_relation(&contact), "unknown");
    contact
        .raw_fields
        .insert("flag".into(), SqlValue::Text("1".into()));
    assert_eq!(contact_relation(&contact), "unknown");
    contact
        .raw_fields
        .insert("flag".into(), SqlValue::Integer("1".into()));
    contact
        .raw_fields
        .insert("delete_flag".into(), SqlValue::Integer("1".into()));
    assert_eq!(contact_relation(&contact), "non_contact");
}

#[test]
fn contact_lists_default_to_friends_and_bind_filters_before_pagination() {
    let (_temp, core) = super::super::groups::tests::fixture();
    set_fields(&core, "cached-outsider", &[("flag", "0")]);
    set_fields(&core, "saved-other", &[("flag", "1")]);
    let (location, keys) = core.profile_location_keys("one").unwrap();
    wechat_profile::apply_contact_patch(
        &location,
        &keys,
        &ContactProjection {
            username: "cached-unknown".into(),
            fields: [("nick_name".into(), SqlValue::Text("Unknown Person".into()))].into(),
        },
        &core.write_options("one", "contact-list-test").unwrap(),
    )
    .unwrap();
    let saved = core
        .catalog_page("one", "contacts", None, &CatalogQuery::default())
        .unwrap();
    assert!(!saved["items"].as_array().unwrap().is_empty());
    assert!(saved["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["relation"] == "friend"));
    for relation in ["non_contact", "unknown"] {
        let page = core
            .catalog_page(
                "one",
                "contacts",
                None,
                &CatalogQuery {
                    relation: Some(relation.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!page["items"].as_array().unwrap().is_empty());
        assert!(page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["relation"] == relation));
    }
    let first = core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let cursor = first["nextCursor"].as_str().unwrap().to_owned();
    let next = core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                cursor: Some(cursor.clone()),
                relation: Some("friend".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(next["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["relation"] == "friend"));
    assert!(core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                cursor: Some(cursor),
                relation: Some("all".into()),
                ..Default::default()
            }
        )
        .is_err());
    let all = core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                relation: Some("all".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(all["items"].as_array().unwrap().len() > saved["items"].as_array().unwrap().len());
    assert_eq!(
        core.catalog_item("one", "contacts", "cached-unknown")
            .unwrap()["relation"],
        "unknown"
    );
    assert!(core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                relation: Some("invalid".into()),
                ..Default::default()
            }
        )
        .is_err());
}
