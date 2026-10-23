//! Typed numbered business messages for the supported native profile.
use crate::{crypto::random, protobuf as p, AccountState, DeviceProfile};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn unix_time() -> Result<u32> {
    Ok(u32::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}
fn text(value: &str, max: usize, empty: bool) -> Result<&[u8]> {
    ensure!(
        (empty || !value.is_empty()) && !value.contains('\0') && value.len() <= max,
        "Invalid text input"
    );
    Ok(value.as_bytes())
}
pub fn base_request(profile: &DeviceProfile, uin: u32, task: u32) -> Result<Vec<u8>> {
    profile.validate()?;
    p::message(&[
        p::bytes(1, &[0]),
        p::uint(2, uin as u64),
        p::bytes(3, &profile.device_id),
        p::int32(4, profile.client_version),
        p::bytes(5, text(&profile.device_type, 1024, false)?),
        p::uint(6, task as u64),
    ])
}
pub fn qr_request(profile: &DeviceProfile, computer_name: &str) -> Result<Vec<u8>> {
    p::message(&[
        p::bytes(1, &base_request(profile, 0, 0)?),
        p::bytes(2, &p::buffer(&random::<16>()?)),
        p::uint(3, 0),
        p::bytes(4, text(computer_name, 255, false)?),
    ])
}
pub fn poll_request(profile: &DeviceProfile, identifier: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !identifier.is_empty() && identifier.len() <= 4096,
        "Invalid login identifier"
    );
    let ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let timestamp = ((ms & 0xffffffff) / 1000) as u64;
    p::message(&[
        p::bytes(1, &base_request(profile, 0, 0)?),
        p::bytes(2, &p::buffer(&random::<16>()?)),
        p::bytes(3, identifier),
        p::uint(4, timestamp),
        p::uint(5, 0),
    ])
}
pub struct QrResponse {
    pub image: Vec<u8>,
    pub identifier: Vec<u8>,
    pub notify_key: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
pub fn decode_qr(input: &[u8]) -> Result<QrResponse> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "QR service returned {code}");
    let image = p::unbuffer(p::data(&f, 2)?)?;
    let identifier = p::data(&f, 3)?.to_vec();
    p::scalar(&f, 4)?;
    let notify_key = p::unbuffer(p::data(&f, 5)?)?;
    ensure!(
        !identifier.is_empty()
            && identifier.len() <= 4096
            && !notify_key.is_empty()
            && notify_key.len() <= 1024,
        "Invalid QR state"
    );
    ensure!(
        image.len() >= 33 && image[..8] == *b"\x89PNG\r\n\x1a\n" && image[12..16] == *b"IHDR",
        "Invalid QR image"
    );
    let width = u32::from_be_bytes(image[16..20].try_into()?);
    let height = u32::from_be_bytes(image[20..24].try_into()?);
    ensure!(
        (21..=4096).contains(&width) && (21..=4096).contains(&height),
        "QR image dimensions invalid"
    );
    Ok(QrResponse {
        image,
        identifier,
        notify_key,
        width,
        height,
    })
}
pub struct PollResponse {
    pub status: Option<u32>,
    pub credential3: Option<Vec<u8>>,
    pub credential4: Option<Vec<u8>>,
    pub lan_challenge: bool,
}
pub fn decode_poll(input: &[u8], notify_key: &[u8]) -> Result<PollResponse> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Login poll returned {code}");
    let Some(pkg) = p::optional_data(&f, 3)? else {
        return Ok(PollResponse {
            status: None,
            credential3: None,
            credential4: None,
            lan_challenge: false,
        });
    };
    let pkg = p::decode(pkg)?;
    let ciphertext = p::unbuffer(p::data(&pkg, 1)?)?;
    p::scalar(&pkg, 2)?;
    let plain = crate::crypto::cbc(&ciphertext, notify_key, openssl::symm::Mode::Decrypt)?;
    let notify = p::decode(&plain)?;
    // LoginQRCodeNotifyData.f17 is an SKBuffer containing
    // LoginQRCodeNonStoreExtNotifyData; its f1 is the risk state.
    let lan_challenge = match p::optional_data(&notify, 17)? {
        Some(buffer) => {
            ensure!(buffer.len() <= 4096, "Login risk extension too large");
            let extension = p::unbuffer(buffer)?;
            let extension = p::decode(&extension)?;
            match p::optional(&extension, 1)? {
                Some(p::Value::Varint(state)) => *state == 11,
                None => false,
                _ => bail!("Invalid login risk state"),
            }
        }
        None => false,
    };
    let status = u32::try_from(p::scalar(&notify, 2)?)?;
    let credential3 = p::optional_data(&notify, 3)?.map(Vec::from);
    let credential4 = p::optional_data(&notify, 4)?.map(Vec::from);
    if status == 2 && !lan_challenge {
        ensure!(
            credential3
                .as_ref()
                .is_some_and(|b| !b.is_empty() && b.len() <= 65536)
                && credential4
                    .as_ref()
                    .is_some_and(|b| !b.is_empty() && b.len() <= 65536),
            "Approved login lacks credentials"
        );
    }
    Ok(PollResponse {
        status: Some(status),
        credential3,
        credential4,
        lan_challenge,
    })
}
pub fn manual_auth(
    profile: &DeviceProfile,
    computer_name: &str,
    c3: &[u8],
    c4: &[u8],
    point: &[u8],
    key_info: &[u8],
) -> Result<Vec<u8>> {
    ensure!(
        !c3.is_empty()
            && !c4.is_empty()
            && c3.len() <= 65536
            && c4.len() <= 65536
            && point.len() == 57
            && point[0] == 4
            && key_info.len() <= 1024 * 1024,
        "Invalid manual auth inputs"
    );
    let rsa = p::message(&[
        p::bytes(1, &p::buffer(&random::<16>()?)),
        p::bytes(
            2,
            &p::message(&[p::int32(1, 713), p::bytes(2, &p::buffer(point))])?,
        ),
        p::bytes(3, c3),
        p::bytes(4, c4),
        p::bytes(5, &[]),
    ])?;
    let wt = p::message(&[p::bytes(1, &[]), p::bytes(4, &p::buffer(&[0]))])?;
    let mut base = vec![p::bytes(2, &wt)];
    if !key_info.is_empty() {
        base.push(p::bytes(5, &p::buffer(key_info)));
    }
    let aes = p::message(&[
        p::bytes(1, &base_request(profile, 0, 1)?),
        p::bytes(2, &p::message(&base)?),
        p::uint(5, 0),
        p::bytes(8, text(computer_name, 255, false)?),
        p::bytes(9, profile.device_type.as_bytes()),
        p::bytes(11, &[]),
        p::int32(13, 1000),
        p::bytes(19, b"0"),
        p::uint(22, 2),
    ])?;
    p::message(&[p::bytes(1, &rsa), p::bytes(2, &aes)])
}
pub fn auto_auth(state: &AccountState, point: &[u8], computer_name: &str) -> Result<Vec<u8>> {
    state.validate()?;
    ensure!(point.len() == 57 && point[0] == 4, "Invalid P224 point");
    let token = p::decode(&state.auto_auth_key)?;
    let part1 = p::unbuffer(p::data(&token, 1)?)?;
    let part2 = p::unbuffer(p::data(&token, 2)?)?;
    ensure!(
        part1.len() == 32 && !part2.is_empty(),
        "Unsupported saved auto-auth token"
    );
    let wt = p::message(&[p::bytes(4, &p::buffer(&[0]))])?;
    let mut base_auth = vec![p::bytes(2, &wt)];
    if !state.key_info.is_empty() {
        base_auth.push(p::bytes(5, &p::buffer(&state.key_info)));
    }
    let aes = p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 12)?),
        p::bytes(2, &p::message(&base_auth)?),
        p::bytes(3, &p::buffer(&state.auto_auth_key)),
        p::uint(6, 0),
        p::bytes(9, text(computer_name, 255, true)?),
        p::bytes(10, state.device.device_type.as_bytes()),
        p::bytes(11, b"en"),
        p::bytes(12, &[]),
        p::bytes(19, state.device.device_type.as_bytes()),
    ])?;
    let rsa = p::message(&[
        p::bytes(2, &p::buffer(&part1)),
        p::bytes(
            3,
            &p::message(&[p::int32(1, 713), p::bytes(2, &p::buffer(point))])?,
        ),
    ])?;
    p::message(&[p::bytes(1, &rsa), p::bytes(2, &aes)])
}
pub fn push_login(
    profile: &DeviceProfile,
    username: &str,
    token: &[u8],
    public_key: &[u8],
) -> Result<Vec<u8>> {
    ensure!(
        !token.is_empty() && token.len() <= 1024 * 1024,
        "Missing saved token"
    );
    p::message(&[
        p::bytes(1, &base_request(profile, 0, 0)?),
        p::bytes(2, text(username, 128, false)?),
        p::bytes(4, &random::<32>()?),
        p::bytes(5, &p::buffer(&random::<16>()?)),
        p::uint(6, 3),
        p::bytes(8, &p::buffer(token)),
        p::bytes(10, &p::buffer(public_key)),
        p::uint(12, 1),
        p::bytes(13, b"en"),
        p::uint(14, 0),
        p::uint(15, 0),
    ])
}
pub fn decode_push(input: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Saved-device login returned {code}");
    let id = p::data(&f, 2)?.to_vec();
    let key = p::unbuffer(p::data(&f, 3)?)?;
    ensure!(
        id.len() == 20 && key.len() == 16,
        "Unsupported saved-device login state"
    );
    Ok((id, key))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SyncCommand {
    pub id: i32,
    pub payload: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SyncBatch {
    pub code: i32,
    pub flags: u32,
    pub cursor: Vec<u8>,
    pub commands: Vec<SyncCommand>,
    pub continuation: bool,
}
pub fn sync_request(cursor: &[u8], device_type: &str, scene: u32) -> Result<Vec<u8>> {
    ensure!(
        !cursor.is_empty() && cursor.len() <= 65536,
        "Invalid sync cursor"
    );
    p::message(&[
        p::bytes(1, &p::uint(1, 0)),
        p::uint(2, 7),
        p::bytes(3, &p::buffer(cursor)),
        p::uint(4, scene as u64),
        p::bytes(5, text(device_type, 1024, false)?),
        p::uint(9, 0),
    ])
}
pub fn decode_sync(input: &[u8], prior: &[u8]) -> Result<SyncBatch> {
    let f = p::decode(input)?;
    let code = p::scalar(&f, 1)? as u32 as i32;
    ensure!(code == 0 || code == -2006, "Sync service returned {code}");
    let list = p::decode(p::data(&f, 2)?)?;
    let flags = u32::try_from(p::scalar(&f, 3)?)?;
    let incoming = p::unbuffer(p::data(&f, 4)?)?;
    if !incoming.is_empty() {
        decode_cursor(&incoming)?;
    }
    let mut commands = vec![];
    for field in list.iter().filter(|f| f.number == 2) {
        let p::Value::Bytes(v) = &field.value else {
            bail!("Invalid sync command");
        };
        let item = p::decode(v)?;
        commands.push(SyncCommand {
            id: p::scalar(&item, 1)? as u32 as i32,
            payload: p::unbuffer(p::data(&item, 2)?)?,
        });
    }
    ensure!(
        p::scalar(&list, 1)? == commands.len() as u64,
        "Sync command count mismatch"
    );
    let cursor = apply_cursor(prior, &incoming, code)?;
    let sent = if prior.is_empty() { &[0][..] } else { prior };
    let continuation = flags & 7 != 0 && incoming != sent;
    Ok(SyncBatch {
        code,
        flags,
        cursor,
        commands,
        continuation,
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AddMessage {
    pub from: String,
    pub to: String,
    pub peer: String,
    pub sender_id: Option<String>,
    pub outgoing: bool,
    pub server_message_id: Option<String>,
    pub numbered1: i32,
    pub message_type: i32,
    pub content: Vec<u8>,
    pub content_text: Option<String>,
    pub numbered6: u32,
    pub numbered7: u32,
    pub inline_data: Vec<u8>,
    pub created_at: u32,
    pub msgsource: Option<Vec<u8>>,
    pub opaque11: Option<Vec<u8>>,
    pub numbered13: Option<u32>,
    /// Present field 13 correlated to official server_seq on the supported
    /// profile (319 exact joined records). Missing remains unsupported for
    /// official-store projection, never a fabricated sequence zero.
    pub server_sequence: Option<u32>,
    pub raw: Vec<u8>,
}
/// Decode AddMsg command 5. Fields 6/7 remain numbered. Present field 13 has a
/// bounded official server_seq correlation; this does not enable every message
/// class for official writes. A room's actual sender requires separate, validated
/// group-envelope/member resolution and is deliberately unresolved here.
pub fn decode_add_message(input: &[u8], own_username: &str) -> Result<AddMessage> {
    let f = p::decode(input)?;
    let sk = |n| -> Result<Vec<u8>> {
        let nested = p::decode(p::data(&f, n)?)?;
        Ok(p::optional_data(&nested, 1)?.unwrap_or_default().to_vec())
    };
    let from = String::from_utf8(sk(2)?)?;
    let to = String::from_utf8(sk(3)?)?;
    ensure!(
        !from.is_empty()
            && !to.is_empty()
            && from.len() <= 512
            && to.len() <= 512
            && !own_username.is_empty(),
        "Invalid message identities"
    );
    let message_type = p::scalar(&f, 4)? as u32 as i32;
    let peer = if message_type == 10000 {
        from.clone()
    } else if from == own_username {
        to.clone()
    } else {
        from.clone()
    };
    ensure!(
        from == own_username || to == own_username || peer.ends_with("@chatroom"),
        "Message does not match account"
    );
    let content = sk(5)?;
    let content_text = String::from_utf8(content.clone()).ok();
    let server_message_id = match p::optional(&f, 12)? {
        Some(p::Value::Varint(v)) => {
            if *v == 0 {
                None
            } else {
                Some(v.to_string())
            }
        }
        None => None,
        _ => bail!("Invalid message server ID"),
    };
    let numbered13 = match p::optional(&f, 13)? {
        Some(p::Value::Varint(v)) => Some(u32::try_from(*v)?),
        None => None,
        _ => bail!("Invalid numbered field13"),
    };
    Ok(AddMessage {
        sender_id: (!from.ends_with("@chatroom")).then(|| from.clone()),
        outgoing: from == own_username,
        from,
        to,
        peer,
        server_message_id,
        numbered1: p::scalar(&f, 1)? as u32 as i32,
        message_type,
        content,
        content_text,
        numbered6: u32::try_from(p::scalar(&f, 6)?)?,
        numbered7: u32::try_from(p::scalar(&f, 7)?)?,
        inline_data: p::unbuffer(p::data(&f, 8)?)?,
        created_at: u32::try_from(p::scalar(&f, 9)?)?,
        msgsource: p::optional_data(&f, 10)?.map(Vec::from),
        opaque11: p::optional_data(&f, 11)?.map(Vec::from),
        numbered13,
        server_sequence: numbered13,
        raw: input.to_vec(),
    })
}
pub fn decode_cursor(input: &[u8]) -> Result<Vec<(u32, u32)>> {
    let f = p::decode(input)?;
    u32::try_from(p::scalar(&f, 1)?)?;
    let mut out = vec![];
    for item in f.iter().filter(|f| f.number == 2) {
        let p::Value::Bytes(v) = &item.value else {
            bail!("Invalid cursor entry");
        };
        let nested = p::decode(v)?;
        out.push((
            u32::try_from(p::scalar(&nested, 1)?)?,
            u32::try_from(p::scalar(&nested, 2)?)?,
        ));
    }
    Ok(out)
}
pub fn apply_cursor(prior: &[u8], incoming: &[u8], code: i32) -> Result<Vec<u8>> {
    if code == -2006 {
        return Ok(incoming.to_vec());
    }
    ensure!(code == 0, "Unsupported sync status");
    if incoming.is_empty() {
        return Ok(prior.to_vec());
    }
    if prior.is_empty() {
        return Ok(incoming.to_vec());
    }
    let mut merged = BTreeMap::new();
    for (k, v) in decode_cursor(prior)? {
        merged.entry(k).or_insert(v);
    }
    for (k, v) in decode_cursor(incoming)? {
        let value = merged.entry(k).or_insert(0);
        *value = (*value).max(v);
    }
    let mut out = vec![p::uint(1, merged.len() as u64)];
    for (k, v) in merged {
        out.push(p::bytes(
            2,
            &p::message(&[p::uint(1, k as u64), p::uint(2, v as u64)])?,
        ));
    }
    p::message(&out)
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Heartbeat {
    pub next_interval: u32,
    pub selector: u32,
}
pub fn heartbeat_request(state: &AccountState, cursor: &[u8]) -> Result<Vec<u8>> {
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::uint(2, unix_time()? as u64),
        p::bytes(3, &p::buffer(cursor)),
    ])
}
pub fn decode_heartbeat(input: &[u8]) -> Result<Heartbeat> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Heartbeat returned {code}");
    Ok(Heartbeat {
        next_interval: u32::try_from(p::scalar(&f, 2)?)?,
        selector: u32::try_from(p::scalar(&f, 3)?)?,
    })
}

pub fn text_send_request(
    destination: &str,
    content: &str,
    created_at: u32,
    client_id: u32,
) -> Result<Vec<u8>> {
    ensure!(
        created_at > 0 && client_id > 0,
        "Invalid send timestamp/client ID"
    );
    let item = p::message(&[
        p::bytes(1, &p::bytes(1, text(destination, 256, false)?)),
        p::bytes(2, text(content, 64 * 1024, false)?),
        p::uint(3, 1),
        p::uint(4, created_at as u64),
        p::uint(5, client_id as u64),
    ])?;
    p::message(&[p::uint(1, 1), p::bytes(2, &item)])
}
/// Native SuperFastHash of the persisted task client-ID string. Tail bytes have
/// signed int8 semantics in the supported AMD64 profile. This is not a hash of
/// message content and is not a server idempotency guarantee.
pub fn client_message_id(data: &[u8]) -> u32 {
    if data.is_empty() || data.len() > i32::MAX as usize {
        return 0;
    }
    let word = |i| u16::from_le_bytes([data[i], data[i + 1]]) as u32;
    let signed = |i| data[i] as i8 as i32 as u32;
    let mut h = data.len() as u32;
    let mut p = 0;
    for _ in 0..data.len() / 4 {
        h = h.wrapping_add(word(p));
        let tmp = (word(p + 2) << 11) ^ h;
        h = (h << 16) ^ tmp;
        p += 4;
        h = h.wrapping_add(h >> 11);
    }
    match data.len() & 3 {
        3 => {
            h = h.wrapping_add(word(p));
            h = (h << 16) ^ h ^ (signed(p + 2) << 18);
            h = h.wrapping_add(h >> 11);
        }
        2 => {
            h = h.wrapping_add(word(p));
            h ^= h << 11;
            h = h.wrapping_add(h >> 17);
        }
        1 => {
            h = h.wrapping_add(signed(p));
            h ^= h << 10;
            h = h.wrapping_add(h >> 1);
        }
        _ => {}
    }
    h ^= h << 3;
    h = h.wrapping_add(h >> 5);
    h ^= h << 4;
    h = h.wrapping_add(h >> 17);
    h ^= h << 25;
    h.wrapping_add(h >> 6)
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct TextSendResult {
    pub accepted: bool,
    pub outer_code: i32,
    pub item_code: Option<i32>,
    pub server_message_id: Option<String>,
    pub returned_destination_matches: bool,
    pub returned_client_id_matches: bool,
}
pub fn decode_text_send(input: &[u8], destination: &str, client_id: u32) -> Result<TextSendResult> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    let declared = p::scalar(&f, 2)?;
    if code != 0 {
        return Ok(TextSendResult {
            accepted: false,
            outer_code: code,
            item_code: None,
            server_message_id: None,
            returned_destination_matches: false,
            returned_client_id_matches: false,
        });
    }
    let list: Vec<_> = f.iter().filter(|f| f.number == 3).collect();
    ensure!(
        declared == 1 && list.len() == 1,
        "Send response cardinality differs"
    );
    let p::Value::Bytes(raw) = &list[0].value else {
        bail!("Invalid send response");
    };
    let item = p::decode(raw)?;
    let item_code = p::scalar(&item, 1)? as u32 as i32;
    let returned = p::decode(p::data(&item, 2)?)?;
    let dest = p::data(&returned, 1)?;
    for n in 3..=7 {
        u32::try_from(p::scalar(&item, n)?)?;
    }
    let id = if item_code == 0 {
        let id = p::scalar(&item, 8)?;
        ensure!(id != 0, "Accepted send has no server ID");
        Some(id.to_string())
    } else {
        None
    };
    Ok(TextSendResult {
        accepted: item_code == 0,
        outer_code: code,
        item_code: Some(item_code),
        server_message_id: id,
        returned_destination_matches: dest == destination.as_bytes(),
        returned_client_id_matches: p::scalar(&item, 4)? == client_id as u64,
    })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ContactRecord {
    pub username: String,
    pub nickname: Option<String>,
    pub remark: Option<String>,
    pub alias: Option<String>,
    pub large_avatar_url: Option<String>,
    pub small_avatar_url: Option<String>,
    pub raw: Vec<u8>,
}
pub fn contact_request(state: &AccountState, usernames: &[String]) -> Result<Vec<u8>> {
    ensure!(
        !usernames.is_empty() && usernames.len() <= 2,
        "Supported contact batch is 1 or 2 identities"
    );
    let mut seen = HashSet::new();
    let mut fields = vec![
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::uint(2, usernames.len() as u64),
    ];
    for user in usernames {
        ensure!(seen.insert(user), "Duplicate requested contact");
        fields.push(p::bytes(3, &p::bytes(1, text(user, 256, false)?)));
    }
    fields.extend([p::uint(4, 0), p::uint(6, 0)]);
    p::message(&fields)
}
fn raw_text(bytes: &[u8]) -> Result<String> {
    Ok(std::str::from_utf8(bytes)?.to_string())
}
fn skstring(f: &[p::Field], n: u32) -> Result<Option<String>> {
    let Some(v) = p::optional_data(f, n)? else {
        return Ok(None);
    };
    let nested = p::decode(v)?;
    Ok(Some(raw_text(
        p::optional_data(&nested, 1)?.unwrap_or_default(),
    )?))
}
pub fn decode_contact_record(raw: &[u8]) -> Result<ContactRecord> {
    let f = p::decode(raw)?;
    let username = skstring(&f, 1)?.ok_or_else(|| anyhow::anyhow!("Contact has no identity"))?;
    ensure!(!username.is_empty(), "Empty contact identity");
    let direct =
        |n| -> Result<Option<String>> { p::optional_data(&f, n)?.map(raw_text).transpose() };
    Ok(ContactRecord {
        username,
        nickname: skstring(&f, 2)?,
        remark: skstring(&f, 10)?,
        alias: direct(30)?,
        large_avatar_url: direct(39)?,
        small_avatar_url: direct(40)?,
        raw: raw.to_vec(),
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactLookup {
    pub contacts: Vec<ContactRecord>,
    pub results: Vec<i32>,
}
fn contact_result_code(value: u64) -> Result<i32> {
    let signed = value as u32 as i32;
    ensure!(
        value <= u32::MAX as u64 || (signed < 0 && value == signed as i64 as u64),
        "Invalid per-contact int32 result"
    );
    Ok(signed)
}
pub fn decode_contacts(input: &[u8], requested: &[String]) -> Result<ContactLookup> {
    ensure!(
        input.len() <= crate::MAX_MESSAGE_BYTES,
        "Contact response exceeds bound"
    );
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Contact service returned {code}");
    let mut slots = vec![];
    let mut results = vec![];
    for field in &f {
        match (field.number, &field.value) {
            (3, p::Value::Bytes(raw)) => slots.push(raw.as_slice()),
            (4, p::Value::Varint(n)) => results.push(contact_result_code(*n)?),
            (4, p::Value::Bytes(raw)) => {
                let mut offset = 0;
                while offset < raw.len() {
                    results.push(contact_result_code(p::read_varint(raw, &mut offset)?)?);
                }
            }
            (3 | 4, _) => bail!("Invalid contact response field"),
            _ => {}
        }
    }
    // The declared count includes default ModContact placeholders for rejected
    // slots. Count the wire slots before interpreting their contact identities.
    ensure!(
        p::scalar(&f, 2)? == slots.len() as u64 && slots.len() <= requested.len(),
        "Contact count mismatch"
    );
    ensure!(
        results.is_empty() || results.len() == requested.len(),
        "Contact result count mismatch"
    );
    let aligned = slots.len() == requested.len() && results.len() == requested.len();
    let mut contacts = vec![];
    let mut seen = HashSet::new();
    let mut omitted_unavailable = false;
    for (index, raw) in slots.iter().enumerate() {
        let fields = p::decode(raw)?;
        let identity = p::optional_data(&fields, 1)?;
        // Observed service code1 accompanies an explicitly present, empty
        // SKString slot. It rejects this lookup; it does not prove deletion or
        // permanent unavailability. Other empty/missing/unknown cases fail.
        if identity == Some(&[][..]) && aligned && results[index] == 1 {
            omitted_unavailable = true;
            continue;
        }
        let record = decode_contact_record(raw)?;
        ensure!(
            requested.contains(&record.username) && seen.insert(record.username.clone()),
            "Unrequested or duplicate contact"
        );
        contacts.push((index, record));
    }
    if omitted_unavailable {
        ensure!(
            contacts
                .iter()
                .all(|(index, record)| requested.get(*index) == Some(&record.username)),
            "Unavailable contact slot alignment mismatch"
        );
    }
    Ok(ContactLookup {
        contacts: contacts.into_iter().map(|(_, record)| record).collect(),
        results,
    })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RoomMember {
    pub username: String,
    pub nickname: Option<String>,
    pub room_display_name: Option<String>,
    pub raw: Vec<u8>,
    #[serde(default)]
    pub flags: u32,
    #[serde(default)]
    pub opaque: Option<Vec<u8>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RoomRoster {
    pub room_id: String,
    pub version: u32,
    pub members: Vec<RoomMember>,
    #[serde(default)]
    pub full_member_ids: Option<Vec<String>>,
    #[serde(default)]
    pub owner: Option<String>,
}
pub fn room_request(state: &AccountState, room_id: &str, version: u32) -> Result<Vec<u8>> {
    ensure!(room_id.ends_with("@chatroom"), "Invalid room identity");
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, text(room_id, 256, false)?),
        p::uint(3, version as u64),
    ])
}
pub fn decode_room(input: &[u8], room_id: &str) -> Result<RoomRoster> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Room service returned {code}");
    if let Some(echo) = p::optional_data(&f, 2)? {
        ensure!(echo == room_id.as_bytes(), "Foreign room reply");
    }
    let version = u32::try_from(p::scalar(&f, 3)?)?;
    let member_data = p::decode(p::data(&f, 4)?)?;
    let mut members = vec![];
    let mut seen = HashSet::new();
    for field in member_data.iter().filter(|f| f.number == 2) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid member field");
        };
        let m = p::decode(raw)?;
        let username = raw_text(p::data(&m, 1)?)?;
        ensure!(
            !username.is_empty() && seen.insert(username.clone()),
            "Invalid member identity"
        );
        members.push(RoomMember {
            username,
            nickname: p::optional_data(&m, 2)?.map(raw_text).transpose()?,
            room_display_name: p::optional_data(&m, 3)?.map(raw_text).transpose()?,
            raw: raw.clone(),
            flags: u32::try_from(p::scalar(&m, 6)?)?,
            opaque: p::optional_data(&m, 7)?.map(Vec::from),
        });
    }
    match p::optional(&member_data, 1)? {
        Some(p::Value::Varint(n)) => ensure!(*n == members.len() as u64, "Member count mismatch"),
        None => {}
        _ => bail!("Invalid member count"),
    }
    let mut full_ids = vec![];
    let mut full_seen = HashSet::new();
    for field in f.iter().filter(|v| v.number == 7) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid full member identity");
        };
        let name = raw_text(p::data(&p::decode(raw)?, 1)?)?;
        ensure!(
            !name.is_empty()
                && name.len() <= 256
                && !name.chars().any(|c| c.is_control() || c.is_whitespace())
                && full_seen.insert(name.clone())
                && full_ids.len() < 10000,
            "Invalid full member identity"
        );
        full_ids.push(name);
    }
    let full_member_ids = match p::optional(&f, 6)? {
        Some(p::Value::Varint(n)) => {
            ensure!(
                u32::try_from(*n)? as usize == full_ids.len(),
                "Full member count mismatch"
            );
            Some(full_ids)
        }
        None if full_ids.is_empty() => None,
        _ => bail!("Invalid full member count"),
    };
    match p::optional(&f, 8)? {
        None | Some(p::Value::Varint(0)) => {}
        _ => bail!("Unsupported member deletion metadata"),
    }
    for field in f.iter().filter(|v| v.number == 9) {
        ensure!(
            matches!(&field.value,p::Value::Bytes(raw) if raw.is_empty()),
            "Unsupported member deletion metadata"
        );
    }
    let owner = p::optional_data(&f, 5)?
        .map(raw_text)
        .transpose()?
        .filter(|s| !s.is_empty());
    Ok(RoomRoster {
        room_id: room_id.to_string(),
        version,
        members,
        full_member_ids,
        owner,
    })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct InitContactPage {
    pub contact_sequence: u32,
    pub room_sequence: u32,
    pub more: bool,
    pub usernames: Vec<String>,
}
pub fn init_contacts_request(
    username: &str,
    contact_sequence: u32,
    room_sequence: u32,
) -> Result<Vec<u8>> {
    p::message(&[
        p::bytes(1, text(username, 256, false)?),
        p::uint(2, contact_sequence as u64),
        p::uint(3, room_sequence as u64),
    ])
}
pub fn decode_init_contacts(input: &[u8]) -> Result<InitContactPage> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Contact bootstrap returned {code}");
    let optional_scalar = |n| -> Result<u32> {
        match p::optional(&f, n)? {
            Some(p::Value::Varint(v)) => Ok(u32::try_from(*v)?),
            None => Ok(0),
            _ => bail!("Invalid contact cursor field"),
        }
    };
    let mut seen = HashSet::new();
    let mut usernames = vec![];
    for field in f.iter().filter(|f| f.number == 5) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid contact identifier");
        };
        let user = raw_text(raw)?;
        ensure!(
            !user.is_empty()
                && user.len() <= 512
                && !user.chars().any(|c| c.is_control() || c.is_whitespace())
                && seen.insert(user.clone())
                && usernames.len() < 10000,
            "Invalid contact bootstrap identity"
        );
        usernames.push(user);
    }
    Ok(InitContactPage {
        contact_sequence: optional_scalar(2)?,
        room_sequence: optional_scalar(3)?,
        more: optional_scalar(4)? != 0,
        usernames,
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AvatarBytes {
    pub username: String,
    pub bytes: Vec<u8>,
}
pub fn avatar_request(state: &AccountState, usernames: &[String]) -> Result<Vec<u8>> {
    ensure!(
        !usernames.is_empty() && usernames.len() <= 2,
        "Supported avatar batch is 1 or 2 identities"
    );
    let mut parts = vec![
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::uint(2, usernames.len() as u64),
    ];
    let mut seen = HashSet::new();
    for user in usernames {
        ensure!(seen.insert(user), "Duplicate avatar identity");
        parts.push(p::bytes(3, &p::bytes(1, text(user, 256, false)?)));
    }
    p::message(&parts)
}
/// Returns exact bounded bytes. The service must fully decode the image before
/// treating this response as a displayable avatar or storing an image mime type.
pub fn decode_avatars(input: &[u8], requested: &[String]) -> Result<Vec<AvatarBytes>> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ensure!(code == 0, "Avatar service returned {code}");
    let mut out = vec![];
    let mut seen = HashSet::new();
    for field in f.iter().filter(|f| f.number == 3) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid avatar item");
        };
        let pair = p::decode(raw)?;
        let username =
            skstring(&pair, 2)?.ok_or_else(|| anyhow::anyhow!("Missing avatar identity"))?;
        ensure!(
            requested.contains(&username) && seen.insert(username.clone()),
            "Foreign or duplicate avatar"
        );
        let bytes = p::unbuffer(p::data(&pair, 1)?)?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= 2 * 1024 * 1024,
            "Avatar bytes outside limit"
        );
        out.push(AvatarBytes { username, bytes });
    }
    ensure!(
        p::scalar(&f, 2)? == out.len() as u64 && out.len() <= requested.len(),
        "Avatar count mismatch"
    );
    Ok(out)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct VoiceChunk {
    pub offset: u32,
    pub next_offset: u32,
    pub total_length: u32,
    pub complete: bool,
    pub bytes: Vec<u8>,
    pub echoed_server_id: Option<String>,
    pub offset_matches: bool,
}
fn voice_range(total: u32, offset: u32) -> Result<u32> {
    ensure!(
        total > 0 && total <= crate::MAX_MESSAGE_BYTES as u32 && offset < total,
        "Invalid voice range"
    );
    Ok((total - offset).min(32768))
}
pub fn voice_request(
    state: &AccountState,
    server_id: u64,
    total: u32,
    offset: u32,
) -> Result<Vec<u8>> {
    ensure!(server_id != 0, "Missing voice server ID");
    let length = voice_range(total, offset)?;
    p::message(&[
        p::uint(1, 0),
        p::uint(2, offset as u64),
        p::uint(3, length as u64),
        p::bytes(5, &base_request(&state.device, state.uin, 0)?),
        p::uint(6, server_id),
    ])
}
pub fn decode_voice(input: &[u8], server_id: u64, total: u32, offset: u32) -> Result<VoiceChunk> {
    let requested = voice_range(total, offset)?;
    let f = p::decode(input)?;
    let base = p::decode(p::data(&f, 9)?)?;
    let code = p::scalar(&base, 1)? as u32 as i32;
    ensure!(code == 0, "Voice service returned {code}");
    for n in [1, 2, 3, 5, 8] {
        u32::try_from(p::scalar(&f, n)?)?;
    }
    let bytes = p::unbuffer(p::data(&f, 7)?)?;
    ensure!(
        p::scalar(&f, 3)? == bytes.len() as u64
            && !bytes.is_empty()
            && bytes.len() <= requested as usize,
        "Voice response length invalid"
    );
    let echoed = match p::optional(&f, 11)? {
        Some(p::Value::Varint(id)) => Some(*id),
        None => None,
        _ => bail!("Invalid voice server ID echo"),
    };
    ensure!(
        echoed.is_none_or(|id| id == server_id),
        "Foreign voice server ID"
    );
    let next_offset = offset + bytes.len() as u32;
    ensure!(next_offset <= total, "Voice chunk overflow");
    Ok(VoiceChunk {
        offset,
        next_offset,
        total_length: total,
        complete: next_offset == total,
        bytes,
        echoed_server_id: echoed.map(|v| v.to_string()),
        offset_matches: p::scalar(&f, 2)? == offset as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_delta_count_is_independent_of_complete_identity_list() {
        let member = p::message(&[
            p::bytes(1, b"changed"),
            p::uint(6, 0x2000),
            p::bytes(7, b"opaque"),
        ])
        .unwrap();
        let roster = |nested: Vec<u8>, extra: Vec<Vec<u8>>| {
            let mut fields = vec![
                p::bytes(1, &p::uint(1, 0)),
                p::uint(3, 7),
                p::bytes(4, &nested),
            ];
            fields.extend(extra);
            p::message(&fields).unwrap()
        };
        let nested = p::message(&[p::uint(1, 1), p::bytes(2, &member)]).unwrap();
        let full = vec![
            p::uint(6, 2),
            p::bytes(7, &p::bytes(1, b"changed")),
            p::bytes(7, &p::bytes(1, b"unchanged")),
        ];
        let decoded = decode_room(&roster(nested.clone(), full.clone()), "room@chatroom").unwrap();
        assert_eq!(decoded.members.len(), 1);
        assert_eq!(decoded.members[0].flags, 0x2000);
        assert_eq!(
            decoded.members[0].opaque.as_deref(),
            Some(b"opaque".as_slice())
        );
        assert_eq!(decoded.full_member_ids.unwrap(), ["changed", "unchanged"]);
        assert!(decode_room(
            &roster(nested.clone(), vec![p::uint(6, 2)]),
            "room@chatroom"
        )
        .is_err());
        assert!(decode_room(&roster(p::bytes(1, b"wrong type"), vec![]), "room@chatroom").is_err());
        assert!(decode_room(
            &roster(p::bytes(2, &p::bytes(1, b"missing-flags")), vec![]),
            "room@chatroom"
        )
        .is_err());
        let mut unknown_deletion = full;
        unknown_deletion.push(p::uint(8, 1));
        assert!(decode_room(&roster(nested.clone(), unknown_deletion), "room@chatroom").is_err());
        assert!(decode_room(&roster(nested, vec![]), "room@chatroom")
            .unwrap()
            .full_member_ids
            .is_none());
    }
    fn cursor(k: u64, v: u64) -> Vec<u8> {
        p::message(&[
            p::uint(1, 1),
            p::bytes(2, &p::message(&[p::uint(1, k), p::uint(2, v)]).unwrap()),
        ])
        .unwrap()
    }
    #[test]
    fn explicit_code1_contact_slot_does_not_hide_successful_neighbor() {
        let requested = vec!["unavailable".into(), "available".into()];
        let empty = p::bytes(1, &[]);
        let available = p::bytes(1, &p::bytes(1, b"available"));
        for codes in [
            p::bytes(4, &[1, 0]),
            p::message(&[p::uint(4, 1), p::uint(4, 0)]).unwrap(),
        ] {
            let raw = p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::uint(2, 2),
                p::bytes(3, &empty),
                p::bytes(3, &available),
                codes,
            ])
            .unwrap();
            let result = decode_contacts(&raw, &requested).unwrap();
            assert_eq!(result.results, vec![1, 0]);
            assert_eq!(result.contacts.len(), 1);
            assert_eq!(result.contacts[0].username, "available");
            assert!(decode_contact_record(&empty).is_err());
        }
    }
    #[test]
    fn contact_result_overflow_cannot_alias_success_or_unavailable() {
        let requested = vec!["unavailable".into(), "available".into()];
        let empty = p::bytes(1, &[]);
        let available = p::bytes(1, &p::bytes(1, b"available"));
        for overflow in [1u64 << 32, (1u64 << 32) + 1, (1u64 << 32) + u32::MAX as u64] {
            let packed = p::uint(1, overflow)[1..].to_vec();
            for codes in [
                p::message(&[p::uint(4, overflow), p::uint(4, 0)]).unwrap(),
                p::bytes(4, &[packed.as_slice(), &[0]].concat()),
            ] {
                let raw = p::message(&[
                    p::bytes(1, &p::uint(1, 0)),
                    p::uint(2, 2),
                    p::bytes(3, &empty),
                    p::bytes(3, &available),
                    codes,
                ])
                .unwrap();
                assert!(decode_contacts(&raw, &requested).is_err());
            }
        }
        assert_eq!(contact_result_code(0).unwrap(), 0);
        assert_eq!(contact_result_code(1).unwrap(), 1);
        assert_eq!(contact_result_code(u32::MAX as u64).unwrap(), -1);
        assert_eq!(contact_result_code(u64::MAX).unwrap(), -1);
        assert_eq!(
            contact_result_code(i32::MIN as i64 as u64).unwrap(),
            i32::MIN
        );
    }
    #[test]
    fn unexplained_empty_contact_slots_and_unproved_alignment_remain_errors() {
        let ids = vec!["unavailable".into(), "available".into()];
        let empty = p::bytes(1, &[]);
        let available = p::bytes(1, &p::bytes(1, b"available"));
        for codes in [vec![], p::bytes(4, &[0, 0]), p::bytes(4, &[2, 0])] {
            let raw = p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::uint(2, 2),
                p::bytes(3, &empty),
                p::bytes(3, &available),
                codes,
            ])
            .unwrap();
            assert!(decode_contacts(&raw, &ids).is_err());
        }
        let partial = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 1),
            p::bytes(3, &empty),
            p::bytes(4, &[1, 0]),
        ])
        .unwrap();
        assert!(decode_contacts(&partial, &ids).is_err());
        let wrong = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 2),
            p::bytes(3, &empty),
            p::bytes(3, &p::bytes(1, &p::bytes(1, b"unavailable"))),
            p::bytes(4, &[1, 0]),
        ])
        .unwrap();
        assert!(decode_contacts(&wrong, &ids).is_err());
        let absent = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 2),
            p::bytes(3, &[]),
            p::bytes(3, &available),
            p::bytes(4, &[1, 0]),
        ])
        .unwrap();
        assert!(decode_contacts(&absent, &ids).is_err());
    }
    #[test]
    fn cursor_merge_and_reset() {
        assert_eq!(
            decode_cursor(&apply_cursor(&cursor(1, 9), &cursor(1, 2), 0).unwrap()).unwrap(),
            vec![(1, 9)]
        );
        assert_eq!(
            apply_cursor(&cursor(1, 9), &cursor(1, 2), -2006).unwrap(),
            cursor(1, 2)
        );
        assert!(apply_cursor(&[], &[], 3).is_err());
    }
    #[test]
    fn send_u64_ids_lossless() {
        let item = p::message(&[
            p::uint(1, 0),
            p::bytes(2, &p::bytes(1, b"filehelper")),
            p::uint(3, 0),
            p::uint(4, 99),
            p::uint(5, 0),
            p::uint(6, 0),
            p::uint(7, 0),
            p::uint(8, u64::MAX),
        ])
        .unwrap();
        let response = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 1),
            p::bytes(3, &item),
        ])
        .unwrap();
        assert_eq!(
            decode_text_send(&response, "filehelper", 99)
                .unwrap()
                .server_message_id,
            Some(u64::MAX.to_string())
        );
    }
    #[test]
    fn foreign_room_rejected() {
        let response = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::bytes(2, b"foreign@chatroom"),
            p::uint(3, 0),
            p::bytes(4, &p::uint(1, 0)),
        ])
        .unwrap();
        assert!(decode_room(&response, "known@chatroom").is_err());
    }
    #[test]
    fn native_client_id_vectors_include_signed_tail_bytes() {
        for (data, expected) in [
            (b"".to_vec(), 0),
            (b"a".to_vec(), 291415938),
            (b"abc".to_vec(), 3535673738),
            (b"native_test_0123456789".to_vec(), 483784200),
            (vec![255], 0),
            (vec![1, 2, 255], 2571803832),
            ((0..=255).collect(), 3840866583),
        ] {
            assert_eq!(client_message_id(&data), expected);
        }
    }
    #[test]
    fn addmsg_preserves_numbered_fields_and_opaque_source() {
        let payload = p::message(&[
            p::uint(1, 7),
            p::bytes(2, &p::bytes(1, b"room@chatroom")),
            p::bytes(3, &p::bytes(1, b"synthetic-self")),
            p::uint(4, 1),
            p::bytes(5, &p::bytes(1, b"synthetic content")),
            p::uint(6, 123),
            p::uint(7, 456),
            p::bytes(8, &p::buffer(&[])),
            p::uint(9, 999),
            p::bytes(10, b"<msgsource/>"),
            p::bytes(11, &[255, 0]),
            p::uint(12, u64::MAX),
            p::uint(13, 789),
        ])
        .unwrap();
        let decoded = decode_add_message(&payload, "synthetic-self").unwrap();
        assert_eq!(decoded.peer, "room@chatroom");
        assert!(decoded.sender_id.is_none());
        assert_eq!(decoded.server_message_id, Some(u64::MAX.to_string()));
        assert_eq!(decoded.numbered13, Some(789));
        assert_eq!(decoded.opaque11, Some(vec![255, 0]));
        assert_eq!(decoded.raw, payload);
    }
}
