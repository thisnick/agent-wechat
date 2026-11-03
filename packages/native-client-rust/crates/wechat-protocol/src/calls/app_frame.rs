//! Selected call-app task framing. Only the explicitly negotiated crypto0 app
//! session is supported. Host transport authentication and media AEAD are separate.
//! These functions prepare/parse bytes; they do not send or retry an operation.

use super::{
    app_session::{AppAuthority, AppSession},
    signaling::CallOperation,
};
use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};
use std::collections::BTreeMap;

const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADER: usize = 0x2800;

/// Selected SDK/device and host configuration. The app's cloud-device string is
/// distinct from its fresh 32-byte auth device and the host account device ID.
pub struct AppFrameParameters<'a> {
    pub sdk_version: u32,
    pub cloud_device_id: &'a [u8],
    pub host_client_version: u32,
    pub app_config_version: u32,
}

/// A decoded app task header. Values may contain private identities, so no Debug.
pub struct TaskFrame {
    pub command: u32,
    pub integers: BTreeMap<u32, u64>,
    pub strings: BTreeMap<u32, Vec<u8>>,
    pub payload: Vec<u8>,
}

/// Build the Cloud app request, selected AppInstance task frame, then host11421
/// delegate. The full accepted app UIN goes into header2 and22. It is not the
/// host UIN, a request counter or a value to narrow through JavaScript floats.
pub fn prepare_transfer(
    session: &AppSession,
    current: AppAuthority,
    operation: CallOperation,
    inner: &[u8],
    parameters: &AppFrameParameters<'_>,
) -> Result<Vec<u8>> {
    session.validate_authority(current)?;
    i32::try_from(parameters.sdk_version)?;
    std::str::from_utf8(parameters.cloud_device_id)?;
    ensure!(
        !inner.is_empty() && inner.len() <= MAX_BODY,
        "Invalid Cloud operation body"
    );
    ensure!(
        !parameters.cloud_device_id.is_empty() && parameters.cloud_device_id.len() <= 128,
        "Invalid Cloud device identity"
    );
    let base = p::message(&[
        p::uint(1, session.app_uin()),
        p::bytes(2, parameters.cloud_device_id),
        p::uint(3, u64::from(parameters.sdk_version)),
    ])?;
    let cloud = p::message(&[p::bytes(1, &base), p::bytes(2, inner)])?;
    let integers = BTreeMap::from([
        (1, 1),
        (2, session.app_uin()),
        (3, 0),
        (4, 0),
        (5, 0xb1400),
        (6, 15),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 1),
        (11, 0),
        (12, 0),
        (13, 0),
        (17, 0),
        (18, 1),
        (20, 1506),
        (21, 0),
        (22, session.app_uin()),
        (23, 0),
        (25, 0),
        (26, 4),
        (28, u64::from(parameters.app_config_version)),
        (29, 1),
        (30, u64::from(parameters.host_client_version)),
        (31, 0),
    ]);
    let strings = BTreeMap::from([
        (14, vec![]),
        (24, b"wechat".to_vec()),
        (27, session.server_id().to_vec()),
    ]);
    let frame = TaskFrame {
        command: operation.command_id(),
        integers,
        strings,
        payload: lz4_literal(&cloud)?,
    }
    .encode()?;
    p::message(&[
        p::uint(1, 1901),
        p::uint(2, u64::from(operation.command_id())),
        p::bytes(3, b"wechat"),
        p::bytes(4, operation.path().as_bytes()),
        p::bytes(5, &frame),
    ])
}

/// Parse an authenticated successful host11421 CommonResponse body. Every inner
/// result remains independently checked, including delegate/task/app/Cloud.
/// Return the Cloud operation body only after successful complete framing.
pub fn decode_transfer_reply(
    session: &AppSession,
    current: AppAuthority,
    operation: CallOperation,
    delegate: &[u8],
) -> Result<Vec<u8>> {
    session.validate_authority(current)?;
    let fields = decode(delegate)?;
    ensure!(
        signed(&fields, 1, Some(0))? == 0,
        "Cloud delegate operation rejected"
    );
    ensure!(
        p::optional_data(&fields, 3)?.is_none_or(|v| v.is_empty()),
        "Unsupported Cloud delegate extension"
    );
    let task = TaskFrame::decode(required_data(&fields, 2)?, operation.command_id())?;
    let integer = |key: u32| {
        task.integers
            .get(&key)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Missing app task header {key}"))
    };
    ensure!(
        signed_value(integer(4)?)? == 0,
        "Cloud task operation rejected"
    );
    ensure!(
        signed_value(integer(23)?)? == 0,
        "Cloud app operation rejected"
    );
    ensure!(
        integer(7)? == 0 && integer(8)? == 0 && integer(21)? == 0 && integer(25)? == 0,
        "Unsupported app task transform"
    );
    if let Some(uin) = task.integers.get(&22) {
        ensure!(*uin == session.app_uin(), "Cloud app identity changed");
    }
    if let Some(app) = task.strings.get(&24) {
        ensure!(
            app.is_empty() || app == b"wechat",
            "Cloud application changed"
        );
    }
    if let Some(server) = task.strings.get(&27) {
        ensure!(
            server.is_empty() || server == session.server_id(),
            "Cloud app session changed"
        );
    }
    let clear = match integer(26)? {
        0 => task.payload,
        4 => lz4_decode(&task.payload, MAX_BODY)?,
        _ => bail!("Unsupported app compression"),
    };
    let cloud = decode(&clear)?;
    let base = decode(required_data(&cloud, 1)?)?;
    ensure!(signed(&base, 1, None)? == 0, "Cloud operation rejected");
    if let Some(Value::Varint(uin)) = p::optional(&cloud, 2)? {
        ensure!(*uin == session.app_uin(), "Cloud response identity changed");
    } else {
        ensure!(
            p::optional(&cloud, 2)?.is_none(),
            "Invalid Cloud response identity"
        );
    }
    Ok(required_data(&cloud, 3)?.to_vec())
}

impl TaskFrame {
    pub fn encode(&self) -> Result<Vec<u8>> {
        ensure!(
            self.command != 0 && self.payload.len() <= MAX_BODY,
            "Invalid app task command or body"
        );
        ensure!(
            self.integers.len() <= 1024 && self.strings.len() <= 1024,
            "Too many app task headers"
        );
        let mut header = vec![1];
        for (key, value) in &self.integers {
            ensure!(*key != 0, "Invalid app task header key");
            header.extend(p::varint(u64::from(*key)));
            header.extend(p::varint(*value));
        }
        header.push(0);
        for (key, value) in &self.strings {
            ensure!(
                *key != 0 && value.len() <= 1024,
                "Invalid app task byte header"
            );
            header.extend(p::varint(u64::from(*key)));
            header.extend(p::varint(value.len() as u64));
            header.extend(value);
        }
        header.push(0);
        ensure!(header.len() <= MAX_HEADER, "App task header too large");
        header.extend(p::varint(header.len() as u64));
        let total = 16 + header.len() + self.payload.len();
        ensure!(total <= MAX_BODY, "App task frame too large");
        let mut out = Vec::with_capacity(total);
        out.extend((total as u32).to_be_bytes());
        out.extend([0x11, 0x10, 0x07, 0x6d]);
        out.extend(self.command.to_be_bytes());
        out.extend([0; 4]);
        out.extend(header);
        out.extend(&self.payload);
        Ok(out)
    }
    pub fn decode(raw: &[u8], expected_command: u32) -> Result<Self> {
        ensure!(
            (16..=MAX_BODY).contains(&raw.len()),
            "Invalid app task size"
        );
        ensure!(
            u32::from_be_bytes(raw[..4].try_into()?) as usize == raw.len()
                && raw[4..8] == [0x11, 0x10, 0x07, 0x6d]
                && raw[12..16] == [0; 4],
            "Invalid app task prefix"
        );
        let command = u32::from_be_bytes(raw[8..12].try_into()?);
        ensure!(command == expected_command, "Unexpected app task command");
        let header = &raw[16..];
        let mut pos = 0;
        ensure!(
            read(header, &mut pos)? == 1,
            "Unsupported app task header version"
        );
        let mut integers = BTreeMap::new();
        let mut strings = BTreeMap::new();
        let mut previous = 0;
        loop {
            let key = u32::try_from(read(header, &mut pos)?)?;
            if key == 0 {
                break;
            }
            ensure!(
                key > previous && integers.len() < 1024,
                "Invalid app integer header order"
            );
            previous = key;
            integers.insert(key, read(header, &mut pos)?);
            ensure!(pos <= MAX_HEADER, "App header too large");
        }
        previous = 0;
        loop {
            let key = u32::try_from(read(header, &mut pos)?)?;
            if key == 0 {
                break;
            }
            ensure!(
                key > previous && strings.len() < 1024,
                "Invalid app byte header order"
            );
            previous = key;
            let len = usize::try_from(read(header, &mut pos)?)?;
            ensure!(
                len <= 1024 && len <= header.len() - pos,
                "App byte header truncated"
            );
            strings.insert(key, header[pos..pos + len].to_vec());
            pos += len;
            ensure!(pos <= MAX_HEADER, "App header too large");
        }
        let footer = pos;
        ensure!(
            read(header, &mut pos)? == footer as u64 && footer <= MAX_HEADER,
            "Invalid app header footer"
        );
        Ok(Self {
            command,
            integers,
            strings,
            payload: header[pos..].to_vec(),
        })
    }
}

fn read(raw: &[u8], pos: &mut usize) -> Result<u64> {
    let start = *pos;
    let value = p::read_varint(raw, pos)?;
    ensure!(
        raw[start..*pos] == p::varint(value),
        "Noncanonical app header integer"
    );
    Ok(value)
}
fn decode(raw: &[u8]) -> Result<Vec<Field>> {
    ensure!(raw.len() <= MAX_BODY, "App message too large");
    super::signaling::decode_fields(raw)
}
fn required_data(fields: &[Field], number: u32) -> Result<&[u8]> {
    p::optional_data(fields, number)?
        .ok_or_else(|| anyhow::anyhow!("Missing app bytes field {number}"))
}
fn signed_value(value: u64) -> Result<i32> {
    Ok(i32::try_from(value as i64)?)
}
fn signed(fields: &[Field], number: u32, default: Option<i32>) -> Result<i32> {
    match p::optional(fields, number)? {
        Some(Value::Varint(value)) => signed_value(*value),
        None => default.ok_or_else(|| anyhow::anyhow!("Missing app result {number}")),
        _ => bail!("Invalid app result {number}"),
    }
}

/// Raw LZ4 literal block. This is legal block syntax, not a framed LZ4 file.
pub fn lz4_literal(raw: &[u8]) -> Result<Vec<u8>> {
    ensure!(raw.len() <= MAX_BODY, "App compression input too large");
    let mut out = vec![(raw.len().min(15) as u8) << 4];
    if raw.len() >= 15 {
        let mut remaining = raw.len() - 15;
        while remaining >= 255 {
            out.push(255);
            remaining -= 255;
        }
        out.push(remaining as u8);
    }
    out.extend(raw);
    Ok(out)
}
pub fn lz4_decode(raw: &[u8], limit: usize) -> Result<Vec<u8>> {
    ensure!(
        raw.len() <= MAX_BODY && limit <= MAX_BODY && !raw.is_empty(),
        "Invalid app LZ4 size"
    );
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < raw.len() {
        let token = raw[pos];
        pos += 1;
        let literals = length(raw, &mut pos, usize::from(token >> 4), limit)?;
        ensure!(
            literals <= raw.len() - pos && literals <= limit - out.len(),
            "App LZ4 literal exceeds bounds"
        );
        out.extend(&raw[pos..pos + literals]);
        pos += literals;
        if pos == raw.len() {
            return Ok(out);
        }
        ensure!(raw.len() - pos >= 2, "App LZ4 offset truncated");
        let offset = usize::from(u16::from_le_bytes(raw[pos..pos + 2].try_into()?));
        pos += 2;
        ensure!(
            offset > 0 && offset <= out.len(),
            "Invalid app LZ4 backreference"
        );
        let repeated = length(raw, &mut pos, usize::from(token & 15), limit)?
            .checked_add(4)
            .ok_or_else(|| anyhow::anyhow!("App LZ4 length overflow"))?;
        ensure!(
            repeated <= limit - out.len(),
            "App LZ4 output exceeds bound"
        );
        for _ in 0..repeated {
            out.push(out[out.len() - offset]);
        }
    }
    bail!("App LZ4 final literal missing")
}
fn length(raw: &[u8], pos: &mut usize, initial: usize, limit: usize) -> Result<usize> {
    let mut value = initial;
    if initial == 15 {
        loop {
            let next = *raw
                .get(*pos)
                .ok_or_else(|| anyhow::anyhow!("App LZ4 length truncated"))?;
            *pos += 1;
            value = value
                .checked_add(usize::from(next))
                .ok_or_else(|| anyhow::anyhow!("App LZ4 length overflow"))?;
            ensure!(value <= limit, "App LZ4 length exceeds bound");
            if next != 255 {
                break;
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_and_overlapping_lz4_vectors() {
        assert_eq!(lz4_decode(&[0x30, b'a', b'b', b'c'], 3).unwrap(), b"abc");
        assert_eq!(lz4_decode(&[0x10, b'a', 1, 0, 0], 5).unwrap(), b"aaaaa");
        for n in [0, 1, 15, 270, 4096] {
            let v = vec![42; n];
            assert_eq!(lz4_decode(&lz4_literal(&v).unwrap(), n).unwrap(), v);
        }
        assert!(lz4_decode(&[0x10, b'a', 0, 0], 20).is_err());
        assert!(lz4_decode(&[0xf0, 255], 16).is_err());
    }
    #[test]
    fn task_frame_full_width_and_corruption() {
        let task = TaskFrame {
            command: 3361,
            integers: BTreeMap::from([(2, u64::MAX), (22, u64::MAX)]),
            strings: BTreeMap::from([(24, b"wechat".to_vec())]),
            payload: vec![1, 2, 3],
        };
        let raw = task.encode().unwrap();
        let decoded = TaskFrame::decode(&raw, 3361).unwrap();
        assert_eq!(decoded.integers[&2], u64::MAX);
        assert_eq!(decoded.payload, vec![1, 2, 3]);
        assert!(TaskFrame::decode(&raw, 3380).is_err());
        let mut corrupt = raw;
        corrupt[0] = 1;
        assert!(TaskFrame::decode(&corrupt, 3361).is_err());
        let mut noncanonical = vec![0, 0, 0, 21, 0x11, 0x10, 7, 0x6d, 0, 0, 13, 33, 0, 0, 0, 0];
        noncanonical.extend([0x81, 0, 0, 0, 4]);
        assert!(TaskFrame::decode(&noncanonical, 3361).is_err());
    }
}
