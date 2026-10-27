use super::*;

fn fixture(encrypted: bool) -> (tempfile::TempDir, ProfileLocation, KeyFile, WriteOptions) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("account");
    let (location, keys) = if encrypted {
        let (location, keys, _) = initialize_profile(&root, &[42; 32]).unwrap();
        (location, keys)
    } else {
        let path = root.join("db_storage/contact/contact.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(path)
            .unwrap()
            .execute_batch(include_str!("../layout/contact.db.sql"))
            .unwrap();
        (
            ProfileLocation::new(root).unwrap(),
            KeyFile::empty("account"),
        )
    };
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: !encrypted,
        durable_intent_id: "synthetic-business-receipt".into(),
    };
    (directory, location, keys, options)
}

fn input(name: &str) -> ContactProjection {
    ContactProjection {
        username: name.into(),
        fields: RawFields::from([
            ("local_type".into(), SqlValue::Integer("1".into())),
            ("verify_flag".into(), SqlValue::Integer("8".into())),
            ("nick_name".into(), SqlValue::Text("Business".into())),
        ]),
    }
}

fn open(location: &ProfileLocation, keys: &KeyFile, options: &WriteOptions) -> Connection {
    open_write(
        location,
        "contact/contact.db",
        keys,
        options.allow_plaintext,
    )
    .unwrap()
}

#[test]
fn business_projection_uses_one_encrypted_transaction_and_replays_idempotently() {
    let (_directory, location, keys, options) = fixture(true);
    let database = open(&location, &keys, &options);
    database
        .execute(
            "INSERT INTO name2id(rowid,username) VALUES(400,'gh_synthetic')",
            [],
        )
        .unwrap();
    let input = input("gh_synthetic");
    let mut business = BusinessContactProjection {
        service_type: 1,
        accept_type: 7,
        child_type: 2,
        brand_flag: u32::MAX,
        home_url: "https://example.invalid/home".into(),
        external_info: "{}".into(),
        brand_info: "brand".into(),
        brand_icon_url: "https://example.invalid/icon".into(),
        brand_list: "list".into(),
        belong: "organization".into(),
    };
    let applied =
        apply_contact_business_patch(&location, &keys, &input, &business, &options).unwrap();
    assert_eq!(applied.local_id, 400);
    assert!(applied.changed);
    assert_eq!(
        database
            .query_row("SELECT extra_buffer FROM contact WHERE id=400", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap(),
        vec![0x68, 1]
    );
    assert_eq!(
        database
            .query_row(
                "SELECT type,accept_type,child_type,brand_flag,username FROM biz_info WHERE id=400",
                [],
                |row| Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?
                ))
            )
            .unwrap(),
        (1, 7, 2, i64::from(u32::MAX), "gh_synthetic".into())
    );
    assert!(
        !apply_contact_business_patch(&location, &keys, &input, &business, &options)
            .unwrap()
            .changed
    );
    business.service_type = 0;
    assert!(
        apply_contact_business_patch(&location, &keys, &input, &business, &options)
            .unwrap()
            .changed
    );
    assert_eq!(
        database
            .query_row("SELECT extra_buffer FROM contact WHERE id=400", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap(),
        vec![0x68, 0]
    );
}

#[test]
fn ordinary_extra_unknown_bytes_are_retained_while_all_service_values_are_replaced() {
    let (_directory, location, keys, options) = fixture(false);
    let mut contact = input("gh_synthetic");
    let original = [
        0x08, 0x81, 0x00, 0x68, 0, 0x12, 3, b'a', b'b', b'c', 0x68, 1, 0x1d, 1, 2, 3, 4,
    ];
    contact.fields.insert(
        "extra_buffer".into(),
        SqlValue::Blob(STANDARD.encode(original)),
    );
    let business = BusinessContactProjection {
        service_type: -1,
        ..Default::default()
    };
    apply_contact_business_patch(&location, &keys, &contact, &business, &options).unwrap();
    let database = open(&location, &keys, &options);
    let actual: Vec<u8> = database
        .query_row("SELECT extra_buffer FROM contact", [], |row| row.get(0))
        .unwrap();
    let mut expected = vec![
        0x08, 0x81, 0x00, 0x12, 3, b'a', b'b', b'c', 0x1d, 1, 2, 3, 4, 0x68,
    ];
    expected.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1]);
    assert_eq!(actual, expected);
    let unchanged = ContactProjection {
        username: contact.username.clone(),
        fields: RawFields::new(),
    };
    assert!(
        !apply_contact_business_patch(&location, &keys, &unchanged, &business, &options)
            .unwrap()
            .changed
    );
    assert_eq!(
        database
            .query_row("SELECT extra_buffer FROM contact", [], |row| row
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        expected
    );
}

#[test]
fn business_update_replaces_the_complete_model_and_keeps_unrelated_contact_state() {
    let (_directory, location, keys, options) = fixture(false);
    let contact = input("gh_synthetic");
    let business = BusinessContactProjection::default();
    apply_contact_business_patch(&location, &keys, &contact, &business, &options).unwrap();
    let database = open(&location, &keys, &options);
    database.execute_batch("UPDATE biz_info SET version=99,sync_version='old',ext_buffer=X'0102',home_url='old',brand_info='old'; UPDATE contact SET remark='keep',extra_buffer=X'08016800'").unwrap();
    let patch = ContactProjection {
        username: contact.username,
        fields: RawFields::new(),
    };
    assert!(
        apply_contact_business_patch(&location, &keys, &patch, &business, &options)
            .unwrap()
            .changed
    );
    assert_eq!(
        database
            .query_row(
                "SELECT version,sync_version,ext_buffer,home_url,brand_info FROM biz_info",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?
                ))
            )
            .unwrap(),
        (0, String::new(), Vec::new(), String::new(), String::new())
    );
    assert_eq!(
        database
            .query_row("SELECT remark,extra_buffer FROM contact", [], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?
            )))
            .unwrap(),
        ("keep".into(), vec![0x08, 1, 0x68, 0])
    );
    assert!(
        !apply_contact_business_patch(&location, &keys, &patch, &business, &options)
            .unwrap()
            .changed
    );
}

#[test]
fn malformed_extra_and_wrong_scalar_wire_rollback_all_rows() {
    let (_directory, location, keys, options) = fixture(false);
    let business = BusinessContactProjection::default();
    for bytes in [
        vec![0x6a, 0],
        vec![0x68, 0x80],
        vec![
            0x68, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2,
        ],
        vec![0x12, 5, b'a'],
    ] {
        let mut contact = input("gh_synthetic");
        contact.fields.insert(
            "extra_buffer".into(),
            SqlValue::Blob(STANDARD.encode(bytes)),
        );
        assert!(
            apply_contact_business_patch(&location, &keys, &contact, &business, &options).is_err()
        );
        let database = open(&location, &keys, &options);
        for table in ["name2id", "contact", "biz_info"] {
            assert_eq!(
                database
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
    assert!(serde_json::from_value::<BusinessContactProjection>(serde_json::json!({"serviceType":2147483648_i64,"acceptType":0,"childType":0,"brandFlag":0,"homeUrl":"","externalInfo":"","brandInfo":"","brandIconUrl":"","brandList":"","belong":""})).is_err());
}

#[test]
fn unsupported_extra_families_and_missing_business_gate_do_not_write() {
    let (_directory, location, keys, options) = fixture(false);
    for kind in [5, 6, 7, -1] {
        let mut contact = input("gh_synthetic");
        contact
            .fields
            .insert("local_type".into(), SqlValue::Integer(kind.to_string()));
        assert!(apply_contact_business_patch(
            &location,
            &keys,
            &contact,
            &BusinessContactProjection::default(),
            &options
        )
        .is_err());
    }
    for name in ["work@openim", "room@chatroom", "support@kefu.openim"] {
        assert!(apply_contact_business_patch(
            &location,
            &keys,
            &input(name),
            &BusinessContactProjection::default(),
            &options
        )
        .is_err());
    }
    let mut contact = input("gh_synthetic");
    contact
        .fields
        .insert("verify_flag".into(), SqlValue::Integer("0".into()));
    assert!(apply_contact_business_patch(
        &location,
        &keys,
        &contact,
        &BusinessContactProjection::default(),
        &options
    )
    .is_err());
    let database = open(&location, &keys, &options);
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM name2id", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn conflicting_contact_or_business_ids_and_duplicate_names_are_atomic_failures() {
    for table in ["contact", "biz_info"] {
        for duplicate in [false, true] {
            let (_directory, location, keys, options) = fixture(false);
            let database = open(&location, &keys, &options);
            database
                .execute(
                    "INSERT INTO name2id(rowid,username) VALUES(10,'gh_synthetic')",
                    [],
                )
                .unwrap();
            if duplicate {
                database.execute(&format!("INSERT INTO {table}(id,username) VALUES(10,'gh_synthetic'),(11,'gh_synthetic')"),[]).unwrap();
            } else {
                database
                    .execute(
                        &format!("INSERT INTO {table}(id,username) VALUES(10,'unrelated')"),
                        [],
                    )
                    .unwrap();
            }
            assert!(apply_contact_business_patch(
                &location,
                &keys,
                &input("gh_synthetic"),
                &BusinessContactProjection::default(),
                &options
            )
            .is_err());
            let other = if table == "contact" {
                "biz_info"
            } else {
                "contact"
            };
            assert_eq!(
                database
                    .query_row(&format!("SELECT count(*) FROM {other}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                database
                    .query_row("SELECT count(*) FROM name2id", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
}

#[test]
fn incompatible_business_schema_cannot_commit_contact_changes() {
    let (_directory, location, keys, options) = fixture(false);
    let database = open(&location, &keys, &options);
    database
        .execute_batch("ALTER TABLE biz_info ADD COLUMN future_field TEXT")
        .unwrap();
    assert!(matches!(
        apply_contact_business_patch(
            &location,
            &keys,
            &input("gh_synthetic"),
            &BusinessContactProjection::default(),
            &options
        ),
        Err(ProfileError::UnsupportedSchema(_))
    ));
    for table in ["name2id", "contact", "biz_info"] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn failed_business_value_bound_rolls_back_the_contact_patch_and_name_map() {
    let (_directory, location, keys, options) = fixture(false);
    let business = BusinessContactProjection {
        brand_info: "x".repeat(4 * 1024 * 1024 + 1),
        ..Default::default()
    };
    assert!(matches!(
        apply_contact_business_patch(
            &location,
            &keys,
            &input("gh_synthetic"),
            &business,
            &options
        ),
        Err(ProfileError::ResourceLimit)
    ));
    let database = open(&location, &keys, &options);
    for table in ["name2id", "contact", "biz_info"] {
        assert_eq!(
            database
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
