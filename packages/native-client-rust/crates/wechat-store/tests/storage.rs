use std::os::unix::fs::{symlink, PermissionsExt};
use std::{fs, path::Path};
use wechat_store::{
    copy::{copy_profile, inventory},
    Store,
};

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("xwechat_files/account/db_storage/message")).unwrap();
    fs::create_dir_all(root.join(".xwechat/empty")).unwrap();
    fs::write(
        root.join("xwechat_files/account/db_storage/message/message_0.db"),
        b"synthetic opaque database",
    )
    .unwrap();
    fs::write(
        root.join("xwechat_files/account/db_storage/message/message_0.db-wal"),
        b"synthetic stopped WAL",
    )
    .unwrap();
    fs::write(root.join(".xwechat/unknown.bin"), [0, 255, 0, 9]).unwrap();
}
#[test]
fn import_export_preserve_opaque_bytes_metadata_wal_and_safe_links() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fixture(&source);
    symlink("unknown.bin", source.join(".xwechat/link")).unwrap();
    let before = inventory(&source).unwrap();
    let store = Store::open(&t.path().join("data")).unwrap();
    let account = store.import("test_account", &source, true).unwrap();
    assert_eq!(account.storage_mode, "managed");
    assert_eq!(account.schema_compatibility, "unverified");
    assert_eq!(inventory(&account.profile_path).unwrap(), before);
    let exported = t.path().join("exported");
    store.export("test_account", &exported, true).unwrap();
    assert_eq!(inventory(&exported).unwrap(), before);
    assert_eq!(inventory(&source).unwrap(), before);
    assert!(!exported.join("agent").exists());
    assert!(store
        .root
        .join("accounts/test_account/agent/control.sqlite")
        .is_file());
}
#[test]
fn direct_attach_preserves_source_and_persists_binding_without_copy() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fixture(&source);
    let before = inventory(&source).unwrap();
    let root = t.path().join("data");
    let store = Store::open(&root).unwrap();
    let id = store.server_id.clone();
    store.attach("direct", &source, true).unwrap();
    assert_eq!(inventory(&source).unwrap(), before);
    assert!(!root.join("accounts/direct/profile").exists());
    drop(store);
    let store = Store::open(&root).unwrap();
    assert_eq!(store.server_id, id);
    assert_eq!(store.accounts().unwrap()[0].profile_path, source);
}
#[test]
fn ownership_conflicts_and_shared_profile_leases_are_detected() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("data");
    let store = Store::open(&root).unwrap();
    assert!(Store::open(&root).is_err());
    let source = t.path().join("source");
    fixture(&source);
    store.attach("one", &source, true).unwrap();
    let leases = store.profile_leases().unwrap();
    let other = Store::open(&t.path().join("other")).unwrap();
    assert!(other.attach("two", &source, true).is_err());
    assert!(copy_profile(&source, &t.path().join("copy"), true).is_err());
    drop(leases);
    other.attach("two", &source, true).unwrap();
}
#[test]
fn copies_require_explicit_stable_source_and_never_replace_destinations() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fixture(&source);
    let destination = t.path().join("output");
    assert!(copy_profile(&source, &destination, false).is_err());
    assert!(!destination.exists());
    fs::create_dir(&destination).unwrap();
    assert!(copy_profile(&source, &destination, true).is_err());
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert!(copy_profile(&source, &source.join("nested"), true).is_err());
}
#[test]
fn escaping_links_special_files_and_duplicate_accounts_fail_without_publishing() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fixture(&source);
    symlink("../../outside", source.join(".xwechat/escape")).unwrap();
    let store = Store::open(&t.path().join("data")).unwrap();
    assert!(store.import("unsafe", &source, true).is_err());
    assert!(store.accounts().unwrap().is_empty());
    assert!(!store.root.join("accounts/unsafe").exists());
    fs::remove_file(source.join(".xwechat/escape")).unwrap();
    store.attach("first", &source, true).unwrap();
    assert!(store.attach("second", &source, true).is_err());
    assert!(store.attach("../escape", &source, true).is_err());
    assert_eq!(store.accounts().unwrap().len(), 1);
}
#[test]
fn newer_or_unrecognized_sidecar_schema_is_preserved() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("data");
    let store = Store::open(&root).unwrap();
    drop(store);
    let path = root.join("registry.sqlite");
    for version in [99, -1] {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        let bytes = fs::read(&path).unwrap();
        assert!(Store::open(&root).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}
#[test]
fn symlink_registry_and_world_readable_data_root_are_rejected() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("data");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(&root).is_err());
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let other = t.path().join("other");
    fs::write(&other, b"preserve").unwrap();
    symlink(&other, root.join("registry.sqlite")).unwrap();
    assert!(Store::open(&root).is_err());
    assert_eq!(fs::read(other).unwrap(), b"preserve");
}

#[test]
fn overlapping_source_is_rejected_before_control_files_are_created() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fixture(&source);
    let before = inventory(&source).unwrap();
    assert!(wechat_store::validate_source_placement(&source.join("new-data"), &source).is_err());
    assert_eq!(inventory(&source).unwrap(), before);
}

#[test]
fn zero_version_does_not_authorize_replacing_a_foreign_schema() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("data");
    wechat_store::private_dir(&root).unwrap();
    let path = root.join("registry.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE preserve_me(value TEXT); INSERT INTO preserve_me VALUES('synthetic');",
    )
    .unwrap();
    drop(db);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&path).unwrap();
    let error = Store::open(&root).err().unwrap();
    assert!(error.to_string().contains("unrecognized registry schema"));
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn full_home_and_direct_account_bindings_share_database_ownership() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("official-home");
    let account = home.join("xwechat_files/synthetic-account");
    std::fs::create_dir_all(account.join("db_storage/message")).unwrap();
    std::fs::write(
        account.join("db_storage/message/message_0.db"),
        b"synthetic opaque bytes",
    )
    .unwrap();
    let first = wechat_store::profile_lease(&home).unwrap();
    assert!(wechat_store::profile_lease(&account).is_err());
    assert!(wechat_store::profile_lease(&account.join("db_storage")).is_err());
    drop(first);
    let second = wechat_store::profile_lease(&account).unwrap();
    assert!(wechat_store::profile_lease(&home).is_err());
    drop(second);
    assert!(wechat_store::profile_lease(&home).is_ok());
    assert_eq!(
        std::fs::read(account.join("db_storage/message/message_0.db")).unwrap(),
        b"synthetic opaque bytes"
    );
}

#[test]
fn official_home_exports_only_wechat_trees_with_sidecars_elsewhere_in_home() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    fixture(&home);
    fs::write(home.join("unrelated-private-file"), b"must not export").unwrap();
    let before = inventory(&home.join("xwechat_files")).unwrap();
    let store = Store::open(&home.join(".local/share/agent-wechat")).unwrap();
    let (account, lease) = store.attach_official_home("one", &home).unwrap();
    assert_eq!(account.storage_mode, "official");
    assert!(store.attach_official_home("two", &home).is_err());
    drop(lease);
    let destination = home.join("wechat-export");
    let manifest = store.export("one", &destination, true).unwrap();
    assert!(manifest
        .entries
        .iter()
        .all(|e| e.path.starts_with("xwechat_files") || e.path.starts_with(".xwechat")));
    assert_eq!(
        inventory(&destination.join("xwechat_files")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(destination.join(".xwechat/unknown.bin")).unwrap(),
        [0, 255, 0, 9]
    );
    assert!(!destination.join(".local").exists());
    assert!(!destination.join("unrelated-private-file").exists());
    assert!(store
        .export("one", &home.join("xwechat_files/nested-export"), true)
        .is_err());
    drop(store);
    let store = Store::open_existing(&home.join(".local/share/agent-wechat")).unwrap();
    assert_eq!(store.accounts().unwrap()[0].storage_mode, "official");
    let overlapping = Store::open(&home.join("xwechat_files/native-sidecars")).unwrap();
    assert!(overlapping.attach_official_home("invalid", &home).is_err());
    assert!(overlapping.accounts().unwrap().is_empty());
}
