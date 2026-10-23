use super::*;

/// Extract only the proved field-control IDs. Version ordering is deliberately
/// not guessed: callers preserve these fields and retain the unresolved update.
pub fn controlled_contact_fields(raw: &[u8]) -> Result<Vec<u32>> {
    let f = p::decode(raw)?;
    let Some(raw) = p::optional_data(&f, 89)? else {
        return Ok(vec![]);
    };
    let list = p::decode(raw)?;
    let mut controls = 0u32;
    let mut ids = std::collections::BTreeSet::new();
    for field in list.iter().filter(|f| f.number == 2) {
        controls += 1;
        ensure!(controls <= 256, "contact_control_limit");
        let p::Value::Bytes(raw) = &field.value else {
            bail!("invalid_contact_control");
        };
        let info = p::decode(raw)?;
        for n in [1, 3] {
            if let Some(value) = p::optional(&info, n)? {
                let p::Value::Varint(v) = value else {
                    bail!("invalid_contact_control");
                };
                u32::try_from(*v)?;
            }
        }
        if let Some(value) = p::optional(&info, 2)? {
            ensure!(
                matches!(value, p::Value::Varint(_)),
                "invalid_contact_control"
            );
        }
        let mut count = 0u32;
        for field in info.iter().filter(|f| f.number == 4) {
            count += 1;
            ensure!(count <= 256, "contact_control_limit");
            let p::Value::Bytes(raw) = &field.value else {
                bail!("invalid_contact_control");
            };
            let field = p::decode(raw)?;
            let id = u32::try_from(p::scalar(&field, 1)?)?;
            ensure!(
                field.len() == 1
                    && matches!(
                        id,
                        8 | 10 | 11 | 12 | 27 | 59 | 60 | 61 | 62 | 68 | 83 | 84 | 87
                    ),
                "unsupported_contact_update_control"
            );
            ids.insert(id);
        }
        if let Some(p::Value::Varint(n)) = p::optional(&info, 3)? {
            ensure!(*n == u64::from(count), "contact_control_count_mismatch");
        }
    }
    match p::optional(&list, 1)? {
        None => {}
        Some(p::Value::Varint(n)) => {
            ensure!(*n == u64::from(controls), "contact_control_count_mismatch")
        }
        _ => bail!("invalid_contact_control"),
    }
    Ok(ids.into_iter().collect())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GroupDetail {
    pub group_id: String,
    pub version: u32,
    pub announcement: String,
    pub editor: String,
    pub published_at: u32,
    pub status: u32,
    pub xml: String,
}
pub fn group_detail_request(state: &AccountState, group: &str) -> Result<Vec<u8>> {
    ordinary_identity(group)?;
    ensure!(group.ends_with("@chatroom"), "invalid_group_identity");
    p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, group.as_bytes()),
    ])
}
pub fn decode_group_detail(raw: &[u8], group: &str) -> Result<GroupDetail> {
    let f = p::decode(raw)?;
    ensure!(p::base_code(&f)? == 0, "group_detail_rejected");
    let text = |n| -> Result<String> {
        Ok(std::str::from_utf8(p::optional_data(&f, n)?.unwrap_or_default())?.into())
    };
    let number = |n| -> Result<u32> {
        match p::optional(&f, n)? {
            None => Ok(0),
            Some(p::Value::Varint(n)) => Ok(u32::try_from(*n)?),
            _ => bail!("invalid_group_detail"),
        }
    };
    // The successful detail read uses generated defaults. Omission here means
    // an empty announcement, unlike omission in a sparse ModContact patch.
    Ok(GroupDetail {
        group_id: group.into(),
        version: number(3)?,
        announcement: text(2)?,
        editor: text(4)?,
        published_at: number(5)?,
        status: number(6)?,
        xml: text(12)?,
    })
}
impl NativeClient {
    pub fn group_detail_raw(&self, state: &mut AccountState, group: &str) -> Result<Vec<u8>> {
        self.authenticated_rpc(
            state,
            223,
            "/cgi-bin/micromsg-bin/getchatroominfodetail",
            &group_detail_request(state, group)?,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contact_controls_preserve_ids_without_guessing_version_order() {
        let info = |count, id| {
            p::message(&[
                p::uint(1, 1),
                p::uint(2, u64::MAX),
                p::uint(3, count),
                p::bytes(4, &p::uint(1, id)),
            ])
            .unwrap()
        };
        let wrap = |entries: Vec<Vec<u8>>| {
            let mut list = vec![p::uint(1, entries.len() as u64)];
            list.extend(entries.iter().map(|e| p::bytes(2, e)));
            p::bytes(89, &p::message(&list).unwrap())
        };
        assert_eq!(
            controlled_contact_fields(&wrap(vec![info(1, 68), info(1, 10), info(1, 10)])).unwrap(),
            [10, 68]
        );
        assert!(controlled_contact_fields(&wrap(vec![info(2, 10)])).is_err());
        assert!(controlled_contact_fields(&wrap(vec![info(1, 999)])).is_err());
        assert!(controlled_contact_fields(&p::bytes(89, &p::bytes(1, b"wrong type"))).is_err());
    }
    #[test]
    fn successful_omission_clears_but_failure_and_wrong_types_do_not() {
        let raw = p::message(&[
            p::bytes(1, &p::int32(1, 0)),
            p::uint(3, 5),
            p::bytes(4, b"editor"),
            p::uint(5, 100),
        ])
        .unwrap();
        let detail = decode_group_detail(&raw, "room@chatroom").unwrap();
        assert_eq!(detail.announcement, "");
        assert_eq!(detail.xml, "");
        assert_eq!(detail.editor, "editor");
        assert_eq!(detail.published_at, 100);
        assert!(decode_group_detail(&p::bytes(1, &p::int32(1, -1)), "room@chatroom").is_err());
        assert!(decode_group_detail(
            &p::message(&[p::bytes(1, &p::uint(1, 0)), p::bytes(5, b"bad")]).unwrap(),
            "room@chatroom"
        )
        .is_err());
    }
}
