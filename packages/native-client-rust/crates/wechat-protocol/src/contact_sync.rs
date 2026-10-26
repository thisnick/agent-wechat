//! Linux 4.1.13.26 contact stream (CGI8674). This stream has its own
//! `rs_config`/`sync_11` cursor; the message SyncKey is only request context.
use crate::{protobuf as p, wire, AccountState, NativeClient};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const CONTACT_SELECTOR: u64 = 0x20004;

#[derive(Clone, Serialize, Deserialize)]
pub struct ContactSyncBatch {
    pub cursor: Vec<u8>,
    pub continuation: Vec<u64>,
    pub commands: Vec<wire::SyncCommand>,
    pub raw: Vec<u8>,
}

/// SyncKeyBuffer: repeated field1 SyncKeyPair { key1:u32, value2:u64 }.
/// Keep the supplied encoding intact, including unknown fields, for publication.
pub fn validate_cursor(input: &[u8]) -> Result<()> {
    ensure!(input.len() <= 65536, "Contact cursor exceeds limit");
    let fields = p::decode(input)?;
    let mut keys = HashSet::new();
    for field in fields.iter().filter(|field| field.number == 1) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid contact cursor pair");
        };
        let pair = p::decode(raw)?;
        let key = u32::try_from(p::scalar(&pair, 1)?)?;
        p::scalar(&pair, 2)?;
        ensure!(
            keys.insert(key) && keys.len() <= 256,
            "Invalid contact cursor keys"
        );
    }
    Ok(())
}

pub fn request(
    state: &AccountState,
    cursor: &[u8],
    message_cursor: &[u8],
    continuation: bool,
) -> Result<Vec<u8>> {
    validate_cursor(cursor)?;
    ensure!(
        message_cursor.len() <= 65536,
        "Message cursor exceeds limit"
    );
    ensure!(
        !cursor.is_empty() || !message_cursor.is_empty(),
        "Contact sync has no cursor context"
    );
    // The drain at 0x6202310 uses scene3 initially and scene4 after a response
    // with nonzero continuation selectors. Request3 is repeated uint64.
    let mut fields = vec![
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::uint(2, if continuation { 4 } else { 3 }),
        p::uint(3, CONTACT_SELECTOR),
        p::bytes(4, cursor),
    ];
    fields.push(p::bytes(5, message_cursor));
    p::message(&fields)
}

pub fn decode(input: &[u8]) -> Result<ContactSyncBatch> {
    ensure!(
        input.len() <= 16 * 1024 * 1024,
        "Contact response exceeds limit"
    );
    let fields = p::decode(input)?;
    let code = p::base_code(&fields)?;
    ensure!(code == 0, "Contact sync service returned {code}");
    let cursor = p::data(&fields, 3)?.to_vec();
    validate_cursor(&cursor)?;
    let mut continuation = Vec::new();
    let mut commands = Vec::new();
    for field in fields {
        match (field.number, field.value) {
            (2, p::Value::Varint(value)) => {
                continuation.push(value);
                ensure!(
                    continuation.len() <= 64,
                    "Contact continuation exceeds limit"
                );
            }
            (4, p::Value::Bytes(raw)) => {
                let item = p::decode(&raw)?;
                // This stream carries raw CmdItem payloads; NewSync uses SKBuffer.
                commands.push(wire::SyncCommand {
                    id: i32::try_from(p::scalar(&item, 1)?)?,
                    payload: p::data(&item, 2)?.to_vec(),
                });
                ensure!(commands.len() <= 10000, "Contact command limit");
            }
            (2 | 4, _) => bail!("Invalid contact response field type"),
            _ => {}
        }
    }
    Ok(ContactSyncBatch {
        cursor,
        continuation,
        commands,
        raw: input.to_vec(),
    })
}

impl NativeClient {
    pub fn contact_sync(
        &self,
        state: &mut AccountState,
        cursor: &[u8],
        message_cursor: &[u8],
        continuation: bool,
    ) -> Result<ContactSyncBatch> {
        let request = request(state, cursor, message_cursor, continuation)?;
        decode(&self.authenticated_rpc(
            state,
            8674,
            "/cgi-bin/micromsg-bin/contactsync",
            &request,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair(key: u64, value: u64) -> Vec<u8> {
        p::bytes(
            1,
            &p::message(&[p::uint(1, key), p::uint(2, value)]).unwrap(),
        )
    }
    #[test]
    fn cursor_values_keep_u64_and_reject_duplicate_or_malformed_keys() {
        validate_cursor(&pair(11, u64::MAX)).unwrap();
        assert!(validate_cursor(&[pair(11, 1), pair(11, 2)].concat()).is_err());
        assert!(validate_cursor(&p::uint(1, 11)).is_err());
        assert!(validate_cursor(&pair(u32::MAX as u64 + 1, 0)).is_err());
    }
    #[test]
    fn final_and_continuing_pages_retain_unknown_commands_and_raw_bytes() {
        for flags in [vec![0], vec![0, CONTACT_SELECTOR]] {
            let mut fields = vec![
                p::bytes(1, &p::message(&[p::int32(1, 0)]).unwrap()),
                p::bytes(3, &pair(11, 7)),
                p::bytes(
                    4,
                    &p::message(&[p::uint(1, 912), p::bytes(2, b"unknown")]).unwrap(),
                ),
            ];
            fields.extend(flags.iter().map(|value| p::uint(2, *value)));
            let raw = p::message(&fields).unwrap();
            let batch = decode(&raw).unwrap();
            assert_eq!(batch.continuation, flags);
            assert_eq!(batch.commands[0].id, 912);
            assert_eq!(batch.commands[0].payload, b"unknown");
            assert_eq!(batch.raw, raw);
        }
    }
}
