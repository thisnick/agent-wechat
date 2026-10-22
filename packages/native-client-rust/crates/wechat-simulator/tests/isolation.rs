use std::{fs, sync::Arc};
use wechat_core::{
    upstream::{Upstream, UpstreamKind, SIMULATOR_MARKER, SIMULATOR_MARKER_CONTENT},
    Core, Store,
};
use wechat_simulator::{adapter::SimulatorUpstream, workspace};

struct Closed;
impl Upstream for Closed {
    fn kind(&self) -> UpstreamKind {
        UpstreamKind::Simulator
    }
}

#[test]
fn native_and_simulated_stores_cannot_be_interchanged() {
    let scratch = wechat_platform::private_tempdir("simulator-isolation-").unwrap();
    let store = Arc::new(Store::open(&scratch.path().join("hub")).unwrap());
    assert!(Core::with_upstream(store.clone(), None, Arc::new(Closed)).is_err());
    fs::write(store.root.join(SIMULATOR_MARKER), SIMULATOR_MARKER_CONTENT).unwrap();
    assert!(Core::new(store.clone(), None).is_err());
    assert!(Core::with_upstream(store, None, Arc::new(Closed)).is_ok());
}

#[test]
fn simulator_refuses_existing_data_and_reset_excludes_running_services() {
    let scratch = wechat_platform::private_tempdir("simulator-workspace-").unwrap();
    let existing = scratch.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("original"), b"preserve this data").unwrap();
    assert!(workspace::prepare(&existing).is_err());
    assert_eq!(
        fs::read(existing.join("original")).unwrap(),
        b"preserve this data"
    );
    let root = workspace::prepare(&scratch.path().join("synthetic state")).unwrap();
    let hub = workspace::hub_directory(&root).unwrap();
    fs::write(hub.join("fixture"), b"synthetic").unwrap();
    let lease = workspace::lease(&root, "hub").unwrap();
    assert!(workspace::reset(&root).is_err());
    assert!(hub.join("fixture").is_file());
    drop(lease);
    workspace::reset(&root).unwrap();
    assert!(!hub.exists());
}

#[test]
fn simulator_accepts_only_numeric_loopback_and_has_no_native_fallback() {
    let token = "synthetic-local-token-01234567890123456789".to_owned();
    assert!(SimulatorUpstream::new("203.0.113.1:80".parse().unwrap(), token.clone()).is_err());
    assert!(SimulatorUpstream::new("127.0.0.1:0".parse().unwrap(), token.clone()).is_err());
    let upstream = SimulatorUpstream::new("127.0.0.1:12345".parse().unwrap(), token).unwrap();
    assert!(upstream
        .native(wechat_protocol::NativeOptions::default())
        .is_err());
}
