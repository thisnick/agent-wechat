use super::*;

const GROUP: &str = "0a0677656368617410001a07766f69702d6d7422002801320a674063686174726f6f6d3800";
const CREATE: &str = "0a01610a016210011a0a674063686174726f6f6d22220801120176180220012a080a0208011202080232030a0144380040004a0208015003288180808080203a07766f69702d6d74";
const ADD: &str =
    "080110021800208180808080202a0a674063686174726f6f6d3201613201623a07766f69702d6d744001";
const CREATE_REPLY: &str = "0801100218002205636c6f75642a05280932016b32090a076d617070696e673a046175746842030a016142080a016210c90148074a1668747470733a2f2f7665726966792e696e76616c69645001980607";
const ADD_REPLY: &str =
    "080122050a0161100022060a016210c9012a1668747470733a2f2f7665726966792e696e76616c6964980607";

fn media<'a>(group: &'a [u8], nics: &'a [Vec<u8>]) -> VoiceInputs<'a> {
    VoiceInputs {
        sdk: &[8, 1, 18, 1, b'v'],
        group,
        device: &[10, 1, b'D'],
        engine_capability: &[8, 1],
        channel_capability: &[8, 2],
        network_type: 2,
        sim_type: 3,
        video_length: 0,
        video_ratio: 0,
        nics,
    }
}
fn room() -> VoiceRoom {
    VoiceRoom {
        room: 1,
        room_key: 2,
        own_member: 0,
    }
}
fn invitees() -> Vec<String> {
    vec!["a".into(), "b".into()]
}

#[test]
fn creator_cancel_keeps_its_own_subtype_original_intent_and_pending_order() {
    let group = hex::decode(GROUP).unwrap();
    let inputs = media(&group, &[]);
    let request = cancel_creation(
        &inputs,
        room(),
        9,
        &["a".into(), "a".into()],
        super::super::personal::CancelReason::Local,
        4,
    )
    .unwrap();
    // Independent tags and scalar bytes for CancelReq nested in the host
    // cancellation wrapper. Member field 7 is intentionally absent here.
    let mut inner = vec![0x0a, 5, 8, 1, 18, 1, b'v', 0x12, group.len() as u8];
    inner.extend_from_slice(&group);
    inner.extend_from_slice(&[0x18, 1, 0x20, 2, 0x28, 9, 0x30, 1]);
    let mut expected = vec![0x0a, inner.len() as u8];
    expected.extend(inner);
    expected.extend_from_slice(&[0x10, 9, 0x18, 1, 0x22, 1, b'a', 0x22, 1, b'a', 0x2a, 7]);
    expected.extend_from_slice(b"voip-mt");
    assert_eq!(request.body(), expected);
    assert_eq!(request.operation.command(), 9332);
    assert!(cancel_creation(
        &inputs,
        room(),
        9,
        &[],
        super::super::personal::CancelReason::Local,
        4
    )
    .is_ok());
    assert!(cancel_creation(
        &inputs,
        room(),
        9,
        &invitees(),
        super::super::personal::CancelReason::NoResponse,
        4
    )
    .is_err());
    assert!(super::super::personal::cancel(
        &inputs,
        1,
        2,
        9,
        &invitees(),
        super::super::personal::CancelReason::Local,
        4
    )
    .is_err());
}

#[test]
fn creation_and_addition_match_independent_wire_fixtures() {
    // Literal fixtures were assembled from field tags/types separately from the
    // Rust builders. They cover repeated strings, nested boundaries, explicit
    // zero fields, an absent business field and a timestamp wider than u32.
    let group = hex::decode(GROUP).unwrap();
    let nics = vec![vec![8, 1]];
    let create = create_voice(
        &media(&group, &nics),
        "g@chatroom",
        &invitees(),
        (1 << 40) + 1,
        7,
        None,
    )
    .unwrap();
    assert_eq!(create.body(), hex::decode(CREATE).unwrap());
    assert_eq!(create.operation.command(), 10342);
    assert_eq!(create.sdk_task, 7);
    let common =
        super::super::signaling::CommonRequest::decode(&create.common(vec![48, 9]).unwrap())
            .unwrap();
    assert_eq!(common.base_request, Some(vec![48, 9]));
    assert_eq!(common.body.as_deref(), Some(create.body()));
    let added = add_members(room(), "g@chatroom", &invitees(), (1 << 40) + 1, 8).unwrap();
    assert_eq!(added.body(), hex::decode(ADD).unwrap());
    assert_eq!(added.operation.command(), 10857);
    assert_eq!(
        added.operation.path(),
        "/cgi-bin/micromsg-bin/voipilinkaddmember"
    );
}

#[test]
fn conference_inputs_do_not_relax_direct_call_validation() {
    let group = hex::decode(GROUP).unwrap();
    let inputs = media(&group, &[]);
    assert!(super::super::personal::invite(&inputs, "a", 1, 1, None).is_err());
    let direct = p::message(&[
        p::bytes(1, b"wechat"),
        p::uint(2, 0),
        p::bytes(3, b"voip-2p"),
        p::bytes(4, b""),
        p::uint(5, 1),
        p::bytes(6, b"g@chatroom"),
        p::uint(7, 0),
    ])
    .unwrap();
    assert!(create_voice(&media(&direct, &[]), "g@chatroom", &invitees(), 1, 1, None).is_err());
    assert!(create_voice(&inputs, "other@chatroom", &invitees(), 1, 1, None).is_err());
}

#[test]
fn optional_custom_auth_and_creation_client_fields_keep_their_presence() {
    let group = hex::decode(GROUP).unwrap();
    let inputs = media(&group, &[]);
    let create = create_voice(&inputs, "g@chatroom", &invitees(), 1, 1, None).unwrap();
    let values = fields(create.body()).unwrap();
    assert!(p::optional_data(&values, 6).unwrap().is_none());
    let client = fields(bytes(&values, 4).unwrap()).unwrap();
    assert!(p::optional_data(&client, 11).unwrap().is_none());
    assert_eq!(optional_u64(&client, 4).unwrap(), Some(1));
    let create = create_voice(&inputs, "g@chatroom", &invitees(), 1, 1, Some(b"auth")).unwrap();
    assert_eq!(
        p::optional_data(&fields(create.body()).unwrap(), 6).unwrap(),
        Some(b"auth".as_slice())
    );
    assert!(create_voice(&inputs, "g@chatroom", &invitees(), 1, 1, Some(&[])).is_err());
}

#[test]
fn request_bounds_and_original_recipient_order_are_checked_without_resubmission() {
    let group = hex::decode(GROUP).unwrap();
    let inputs = media(&group, &[]);
    assert!(create_voice(&inputs, "g@chatroom", &[], 1, 1, None).is_err());
    assert!(create_voice(&inputs, "g@chatroom", &vec!["a".into(); 101], 1, 1, None).is_err());
    assert!(create_voice(&inputs, "g@chatroom", &vec!["a".into(); 100], 1, 1, None).is_ok());
    assert!(add_members(room(), "g@chatroom", &["a".repeat(129)], 1, 1).is_err());
    assert!(add_members(room(), "g@chatroom", &["a".repeat(128)], 1, 1).is_ok());
    assert!(add_members(room(), "g@chatroom", &invitees(), 0, 1).is_err());
    assert!(add_members(
        VoiceRoom {
            room_key: 0,
            ..room()
        },
        "g@chatroom",
        &invitees(),
        1,
        1
    )
    .is_err());
    let request = add_members(
        room(),
        "g@chatroom",
        &["b".into(), "a".into(), "b".into()],
        1,
        1,
    )
    .unwrap();
    let names = fields(request.body())
        .unwrap()
        .into_iter()
        .filter(|field| field.number == 6)
        .map(|field| field.value)
        .collect::<Vec<_>>();
    assert!(
        names
            == vec![
                Value::Bytes(b"b".to_vec()),
                Value::Bytes(b"a".to_vec()),
                Value::Bytes(b"b".to_vec())
            ]
    );
}

#[test]
fn creation_results_preserve_partial_outcomes_auth_verification_and_unknown_fields() {
    let raw = hex::decode(CREATE_REPLY).unwrap();
    let result = CreateResponse::decode(&raw).unwrap();
    assert!(result.allocated_room().unwrap() == Some(room()));
    assert_eq!(result.cloud_group_id.as_deref(), Some("cloud"));
    assert_eq!(result.user_mappings.len(), 1);
    assert_eq!(result.custom_auth.as_deref(), Some(b"auth".as_slice()));
    assert_eq!(result.invite_results.len(), 2);
    assert_eq!(result.invite_results[0].service_code, None);
    assert_eq!(result.invite_results[1].service_code, Some(201));
    assert_eq!(result.invite_results[1].unknown_fields[0].number, 9);
    assert_eq!(
        result.verification_url.as_deref(),
        Some("https://verify.invalid")
    );
    assert_eq!(result.ui_flags, Some(1));
    assert_eq!(result.unknown_fields[0].number, 99);
    assert_eq!(result.original, raw);
}

#[test]
fn addition_results_do_not_fabricate_missing_identity_or_membership() {
    let raw = hex::decode(ADD_REPLY).unwrap();
    let result = AddMembersResponse::decode(&raw).unwrap();
    result.check_room(room()).unwrap();
    assert_eq!(result.room_key, None);
    assert_eq!(result.own_member, None);
    assert_eq!(result.invite_results[0].service_code, Some(0));
    assert_eq!(result.invite_results[1].service_code, Some(201));
    assert_eq!(
        result.verification_url.as_deref(),
        Some("https://verify.invalid")
    );
    assert_eq!(result.unknown_fields[0].number, 99);
    assert_eq!(result.original, raw);
    let mut mismatch = raw.clone();
    mismatch.extend([0x10, 9]);
    assert!(AddMembersResponse::decode(&mismatch)
        .unwrap()
        .check_room(room())
        .is_err());
    assert!(result.check_room(VoiceRoom { room: 4, ..room() }).is_err());
}

#[test]
fn malformed_nested_boundaries_scalar_widths_and_duplicate_identity_fail() {
    for raw in [vec![8, 1, 8, 1], vec![8, 1, 26, 0], vec![8, 1, 34, 1, 0x0a]] {
        assert!(AddMembersResponse::decode(&raw).is_err());
    }
    let raw = p::message(&[p::uint(1, 1), p::uint(3, 1 << 32)]).unwrap();
    assert!(AddMembersResponse::decode(&raw).is_err());
    let raw = p::message(&[
        p::uint(1, 1),
        p::bytes(
            4,
            &p::message(&[p::bytes(1, b"a"), p::bytes(2, b"wrong wire")]).unwrap(),
        ),
    ])
    .unwrap();
    assert!(AddMembersResponse::decode(&raw).is_err());
    let mut create = hex::decode(CREATE_REPLY).unwrap();
    create[3] = 0;
    assert!(CreateResponse::decode(&create)
        .unwrap()
        .allocated_room()
        .is_err());
}

#[test]
fn rejected_responses_keep_verification_and_partial_outcomes_without_allocation() {
    let rejected = p::message(&[
        p::uint(1, 0),
        p::bytes(
            8,
            &p::message(&[p::bytes(1, b"a"), p::uint(2, 201)]).unwrap(),
        ),
        p::bytes(9, b"https://verify.invalid"),
    ])
    .unwrap();
    let result = CreateResponse::decode(&rejected).unwrap();
    assert!(result.allocated_room().unwrap().is_none());
    assert_eq!(result.invite_results[0].service_code, Some(201));
    assert_eq!(
        result.verification_url.as_deref(),
        Some("https://verify.invalid")
    );
    let rejected = p::message(&[p::bytes(5, b"https://verify.invalid")]).unwrap();
    let result = AddMembersResponse::decode(&rejected).unwrap();
    assert_eq!(result.room_id, None);
    assert_eq!(
        result.verification_url.as_deref(),
        Some("https://verify.invalid")
    );
    assert!(result.check_room(room()).is_err());
}

#[test]
fn microphone_switch_uses_audio_bit_monotonic_timestamp_and_present_empty_metadata() {
    let group = hex::decode(GROUP).unwrap();
    let inputs = media(&group, &[]);
    let request = switch_microphone(&inputs, room(), false, 300, 7).unwrap();
    assert_eq!(request.operation().unwrap().command_id(), 3197);
    assert_eq!(
        request.operation().unwrap().path(),
        "/cloudim/cloud-bin/cloudvoipmtswitchav"
    );
    // Independent tag/wire bytes for the voice-only request, including field8.
    let mut expected = vec![0x0a, 5, 8, 1, 18, 1, b'v', 0x12, group.len() as u8];
    expected.extend(&group);
    expected.extend([0x18, 1, 0x20, 2, 0x28, 0, 0x30, 0, 0x38, 0xac, 2, 0x42, 0]);
    assert_eq!(request.body.as_deref().unwrap(), expected);
    assert!(switch_microphone(&inputs, room(), true, 0, 7).is_err());
    assert!(switch_microphone(&inputs, room(), true, 1, 0).is_err());
}
