//! Separate group catalog and lossless read-only metadata response receipts.
use super::*;
use std::collections::HashSet;

#[derive(Clone, Serialize, Deserialize)]
pub struct MetadataReply<T> {
    pub value: T,
    pub raw: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct GroupMetadataPage {
    pub contacts: Vec<ContactRecord>,
    pub next_cursor: Option<Vec<u8>>,
    pub more: bool,
    pub wait_seconds: u32,
    pub raw: Vec<u8>,
}
pub fn group_inventory_request(state: &AccountState, cursor: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !cursor.is_empty() && cursor.len() <= 65536,
        "Invalid raw group cursor"
    );
    p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, cursor),
    ])
}
pub fn decode_group_inventory(raw: &[u8]) -> Result<GroupMetadataPage> {
    let fields = p::decode(raw)?;
    let code = p::base_code(&fields)?;
    ensure!(code == 0, "Group metadata service returned {code}");
    let mut contacts = vec![];
    let mut seen = HashSet::new();
    for field in fields.iter().filter(|f| f.number == 2) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid group contact field")
        };
        let record = wire::decode_contact_record(raw)?;
        ensure!(
            seen.insert(record.username.clone()) && contacts.len() < 5000,
            "Duplicate or excessive group metadata"
        );
        contacts.push(record);
    }
    let next_cursor = p::optional_data(&fields, 3)?.map(Vec::from);
    ensure!(
        next_cursor
            .as_ref()
            .is_none_or(|c| !c.is_empty() && c.len() <= 65536),
        "Invalid next group cursor"
    );
    let control = p::optional_data(&fields, 4)?
        .map(p::decode)
        .transpose()?
        .unwrap_or_default();
    let scalar = |n| -> Result<u32> {
        match p::optional(&control, n)? {
            None => Ok(0),
            Some(p::Value::Varint(n)) => Ok(u32::try_from(*n)?),
            _ => bail!("Invalid group control"),
        }
    };
    let more = scalar(1)? != 0;
    let wait_seconds = scalar(2)?;
    ensure!(
        !more || next_cursor.is_some(),
        "Continuing group page has no cursor"
    );
    Ok(GroupMetadataPage {
        contacts,
        next_cursor,
        more,
        wait_seconds,
        raw: raw.to_vec(),
    })
}
impl NativeClient {
    pub fn group_inventory(
        &self,
        state: &mut AccountState,
        cursor: &[u8],
    ) -> Result<GroupMetadataPage> {
        let request = group_inventory_request(state, cursor)?;
        let raw = self.authenticated_rpc(
            state,
            15595,
            "/cgi-bin/micromsg-bin/syncsimplechatroomdata",
            &request,
        )?;
        let page = decode_group_inventory(&raw)?;
        ensure!(
            !page.more || page.next_cursor.as_deref() != Some(cursor),
            "Group metadata made no cursor progress"
        );
        Ok(page)
    }
    /// Verified native business payload, retained before strict contact decoding.
    /// Callers must protect this account data at rest. The transport's global
    /// message bound applies; no identities or opaque fields are exposed in logs.
    pub fn lookup_contacts_raw(&self, state: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
        let request = wire::contact_request(state, ids)?;
        let raw =
            self.authenticated_rpc(state, 182, "/cgi-bin/micromsg-bin/getcontact", &request)?;
        ensure!(
            raw.len() <= crate::MAX_MESSAGE_BYTES,
            "Contact response exceeds bound"
        );
        Ok(raw)
    }
    pub fn lookup_contacts_receipt(
        &self,
        state: &mut AccountState,
        ids: &[String],
    ) -> Result<MetadataReply<ContactLookup>> {
        let raw = self.lookup_contacts_raw(state, ids)?;
        Ok(MetadataReply {
            value: wire::decode_contacts(&raw, ids)?,
            raw,
        })
    }
    pub fn lookup_room_receipt(
        &self,
        state: &mut AccountState,
        id: &str,
        version: u32,
    ) -> Result<MetadataReply<RoomRoster>> {
        let raw = self.lookup_room_raw(state, id, version)?;
        Ok(MetadataReply {
            value: wire::decode_room(&raw, id)?,
            raw,
        })
    }
    pub fn lookup_room_raw(
        &self,
        state: &mut AccountState,
        id: &str,
        version: u32,
    ) -> Result<Vec<u8>> {
        let request = wire::room_request(state, id, version)?;
        self.authenticated_rpc(
            state,
            551,
            "/cgi-bin/micromsg-bin/getchatroommemberdetail",
            &request,
        )
    }
    pub fn init_contacts_receipt(
        &self,
        state: &mut AccountState,
        a: u32,
        b: u32,
    ) -> Result<MetadataReply<InitContactPage>> {
        let request = wire::init_contacts_request(&state.own_username, a, b)?;
        let raw =
            self.authenticated_rpc(state, 851, "/cgi-bin/micromsg-bin/initcontact", &request)?;
        let value = wire::decode_init_contacts(&raw)?;
        ensure!(
            !value.more || value.contact_sequence != a || value.room_sequence != b,
            "Contact bootstrap made no cursor progress"
        );
        Ok(MetadataReply { value, raw })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_cursor_is_raw_and_control_presence_is_optional() {
        let raw = p::message(&[p::bytes(1, &p::uint(1, 0)), p::bytes(3, &[0, 255, 1])]).unwrap();
        let page = decode_group_inventory(&raw).unwrap();
        assert_eq!(page.next_cursor, Some(vec![0, 255, 1]));
        assert!(!page.more);
        assert_eq!(page.wait_seconds, 0);
        assert_eq!(page.raw, raw);
        let bad = p::message(&[p::bytes(1, &p::uint(1, 0)), p::bytes(4, &p::uint(1, 1))]).unwrap();
        assert!(decode_group_inventory(&bad).is_err());
    }
    #[test]
    fn rejection_does_not_require_success_fields() {
        assert!(decode_group_inventory(&p::bytes(1, &p::int32(1, -13)))
            .err()
            .unwrap()
            .to_string()
            .contains("-13"));
    }
}
