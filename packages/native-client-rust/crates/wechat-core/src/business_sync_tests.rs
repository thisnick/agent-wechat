use super::encrypted_roundtrip::state;
use super::*;
use wechat_protocol::protobuf as p;

fn command(service_type: Option<i32>, verify: bool) -> SyncCommand {
    let mut payload = p::bytes(1, &p::bytes(1, b"gh_synthetic"));
    payload.extend(p::uint(7, u32::MAX.into()));
    payload.extend(p::uint(8, 1));
    if verify {
        payload.extend(p::uint(24, 8));
    }
    if let Some(service_type) = service_type {
        payload.extend(p::bytes(41, b"synthetic-brand-list"));
        let external = format!("{{\"ServiceType\":{service_type}}}");
        payload.extend(p::bytes(42, &p::bytes(2, external.as_bytes())));
    }
    SyncCommand { id: 2, payload }
}

#[test]
fn business_sync_replays_once_and_updates_the_two_canonical_representations() {
    let temporary = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let core = Core::new(store.clone(), None).unwrap();
    core.configure_profile_format_key(&[9; 16]).unwrap();
    core.install_login_state("one", &state(), None).unwrap();
    core.ensure_managed_profile("one", &state()).unwrap();

    let read = || {
        core.catalog_item("one", "contacts", "gh_synthetic")
            .unwrap()["classification"]
            .clone()
    };
    core.apply_sync_command("one", &command(Some(1), true), "business-first", "live")
        .unwrap();
    assert_eq!(read()["category"], "business");
    assert_eq!(read()["businessType"], 1);
    assert_eq!(read()["businessKind"], "service_account");
    let reader = core.profile_reader("one").unwrap();
    let metadata = reader.peer_metadata(&["gh_synthetic".into()]).unwrap();
    assert_eq!(metadata["gh_synthetic"].contact_service_type, Some(1));
    assert_eq!(
        metadata["gh_synthetic"]
            .business
            .as_ref()
            .unwrap()
            .service_type,
        Some(1)
    );
    drop(reader);

    // An unrelated sparse contact update does not invent business defaults.
    core.apply_sync_command("one", &command(None, false), "business-sparse", "live")
        .unwrap();
    assert_eq!(read()["businessType"], 1);

    // An admitted complete business record uses its defaults when extras vanish.
    core.apply_sync_command("one", &command(None, true), "business-reset", "live")
        .unwrap();
    assert_eq!(read()["businessType"], 0);
    assert_eq!(read()["businessKind"], "official_account");
    let events_before = store.events("one", 0, 100).unwrap().len();
    core.apply_sync_command("one", &command(Some(1), true), "business-first", "catchup")
        .unwrap();
    assert_eq!(read()["businessType"], 0);
    assert_eq!(store.events("one", 0, 100).unwrap().len(), events_before);
}
