//! Inspect the active authentication response before selecting a call host RPC.
//! A selected Ax route must not silently fall back to ordinary host packing.
//! This module selects no keys, prepares no request and performs no transport.

use crate::{protobuf as p, AuthSnapshot};
use anyhow::{ensure, Result};
use zeroize::Zeroize;

const MAX_AUTH_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallHostPolicy {
    /// No active per-CGI Ax entry; the host's ordinary transport policy applies.
    Ordinary,
    /// An active Ax entry selects a different body/header policy. An adapter that
    /// does not implement it must return unsupported, never use Ordinary.
    AxSelected,
}

/// Fixed host CGI/path pairs for the selected call service. Cloud operation IDs
/// remain inside the transfer envelope, never substituted for a host CGI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallHostRoute {
    AppAuth,
    RingingAck,
    Transfer,
    Invite,
    Cancel,
    LiveSync,
}
impl CallHostRoute {
    pub const fn command(self) -> u16 {
        match self {
            Self::AppAuth => 7994,
            Self::RingingAck => 8069,
            Self::Transfer => 11421,
            Self::Invite => 10342,
            Self::Cancel => 9332,
            Self::LiveSync => 9262,
        }
    }
    pub const fn path(self) -> &'static str {
        match self {
            Self::AppAuth => "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth",
            Self::RingingAck => "/cgi-bin/micromsg-bin/voipilinkack",
            Self::Transfer => "/cgi-bin/micromsg-bin/mmilinktransfer",
            Self::Invite => "/cgi-bin/micromsg-bin/voipilinkinvite",
            Self::Cancel => "/cgi-bin/micromsg-bin/voipilinkcancel",
            Self::LiveSync => "/cgi-bin/micromsg-bin/voipilinklivesync",
        }
    }
}
/// Identify the supported terminal operations from the owned request itself.
/// A generic transfer is not a teardown: delegate command/path and nested task
/// command must all identify Exit. This does not replace account/room fencing.
pub fn is_teardown_request(route: CallHostRoute, common: &[u8]) -> Result<bool> {
    if route == CallHostRoute::Cancel {
        return Ok(true);
    }
    if route != CallHostRoute::Transfer {
        return Ok(false);
    }
    let common = super::signaling::CommonRequest::decode(common)?;
    if !common.unknown_fields.is_empty() {
        return Ok(false);
    }
    let body = common
        .body
        .ok_or_else(|| anyhow::anyhow!("Missing call transfer body"))?;
    let delegate = Fields::decode(&body)?;
    if delegate.0.iter().any(|f| !(1..=5).contains(&f.number))
        || delegate.required_u64(1)? != 1901
        || delegate.required_u64(2)? != 3380
        || delegate.required_bytes(3)? != b"wechat"
        || delegate.required_bytes(4)? != super::signaling::CallOperation::Exit.path().as_bytes()
    {
        return Ok(false);
    }
    let frame = super::app_frame::TaskFrame::decode(delegate.required_bytes(5)?, 3380)?;
    const INTEGER_KEYS: &[u32] = &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18, 20, 21, 22, 23, 25, 26, 28, 29, 30, 31,
    ];
    if frame.command != 3380
        || frame.integers.keys().any(|n| !INTEGER_KEYS.contains(n))
        || frame.strings.keys().any(|n| ![14, 24, 27].contains(n))
        || frame.integers.get(&1) != Some(&1)
        || frame.integers.get(&26) != Some(&4)
        || [7, 8, 21, 25]
            .iter()
            .any(|n| frame.integers.get(n) != Some(&0))
        || frame.strings.get(&24).map(Vec::as_slice) != Some(b"wechat")
        || frame.strings.get(&27).is_none_or(Vec::is_empty)
    {
        return Ok(false);
    }
    let clear = zeroize::Zeroizing::new(super::app_frame::lz4_decode(&frame.payload, 64 * 1024)?);
    let cloud = Fields::decode(&clear)?;
    if cloud.0.iter().any(|f| ![1, 2].contains(&f.number)) {
        return Ok(false);
    }
    let base = Fields::decode(cloud.required_bytes(1)?)?;
    let uin = base.required_u64(1)?;
    if base.0.iter().any(|f| ![1, 2, 3].contains(&f.number))
        || uin == 0
        || base.required_bytes(2)?.is_empty()
        || i32::try_from(base.required_u64(3)?).is_err()
        || frame.integers.get(&2) != Some(&uin)
        || frame.integers.get(&22) != Some(&uin)
    {
        return Ok(false);
    }
    let exit = Fields::decode(cloud.required_bytes(2)?)?;
    if exit.0.iter().any(|f| !(1..=8).contains(&f.number))
        || exit.required_bytes(1)?.is_empty()
        || exit.required_bytes(2)?.is_empty()
        || exit.required_u64(3)? == 0
        || exit.required_u64(4)? == 0
        || [5, 6, 7].iter().any(|n| {
            exit.required_u64(*n)
                .map_or(true, |v| u32::try_from(v).is_err())
        })
        || exit.required_u64(8)? > 1
    {
        return Ok(false);
    }
    let sdk = Fields::decode(exit.required_bytes(1)?)?;
    if sdk.0.iter().any(|f| ![1, 2].contains(&f.number))
        || u32::try_from(sdk.required_u64(1)?).is_err()
        || sdk.required_bytes(2)?.is_empty()
        || std::str::from_utf8(sdk.required_bytes(2)?).is_err()
    {
        return Ok(false);
    }
    let group = Fields::decode(exit.required_bytes(2)?)?;
    if group.0.iter().any(|f| !(1..=7).contains(&f.number))
        || group.required_bytes(1)? != b"wechat"
        || group.required_bytes(3)? != b"voip-2p"
        || group.required_u64(5)? != 1
    {
        return Ok(false);
    }
    if let Some(value) = crate::protobuf::optional(&group.0, 2)? {
        if !matches!(value, crate::protobuf::Value::Varint(_)) {
            return Ok(false);
        }
    }
    for field in [4, 6] {
        if let Some(value) = group.optional_bytes(field)? {
            if value.len() > 1024 || std::str::from_utf8(value).is_err() {
                return Ok(false);
            }
        }
    }
    group.optional_u32(7)?;
    Ok(true)
}

impl From<super::personal::HostOperation> for CallHostRoute {
    fn from(value: super::personal::HostOperation) -> Self {
        match value {
            super::personal::HostOperation::RingingAck => Self::RingingAck,
            super::personal::HostOperation::Invite => Self::Invite,
            super::personal::HostOperation::Cancel => Self::Cancel,
            super::personal::HostOperation::LiveSync => Self::LiveSync,
        }
    }
}

/// Inspect the current root auth response, not retained `ax_auth` bytes. Presence
/// of a historical section does not activate it when the current update mask
/// excludes it. Callers must still fence account/logout generations and honor
/// any separately configured host override before dispatching.
pub fn selected_policy(snapshot: &AuthSnapshot, command: u16) -> Result<CallHostPolicy> {
    ensure!(
        matches!(command, 7994 | 8069 | 11421 | 10342 | 9332 | 9262),
        "Unsupported call host command"
    );
    let root = Fields::decode(&snapshot.response)?;
    let base = Fields::decode(root.required_bytes(1)?)?;
    ensure!(
        i32::try_from(base.required_u64(1)? as i64)? == 0,
        "Rejected call auth snapshot"
    );
    let mask = u32::try_from(root.required_u64(2)?)?;
    ensure!(
        mask == snapshot.update_mask,
        "Call auth snapshot mask mismatch"
    );
    if mask & 8 == 0 {
        return Ok(CallHostPolicy::Ordinary);
    }
    let Some(section) = root.optional_bytes(6)? else {
        return Ok(CallHostPolicy::Ordinary);
    };
    let list = Fields::decode(section)?;
    let mut count = 0;
    let mut selected = CallHostPolicy::Ordinary;
    for field in &list.0 {
        if field.number != 2 {
            continue;
        }
        count += 1;
        ensure!(count <= MAX_ENTRIES, "Too many call host auth entries");
        let p::Value::Bytes(raw) = &field.value else {
            anyhow::bail!("Invalid call host auth entry wire type");
        };
        let entry = Fields::decode(raw)?;
        // Match native scalar defaults and validate known nested wire shapes.
        entry.optional_u32(1)?;
        let cgi = entry.optional_u32(4)?;
        entry.optional_bytes(3)?;
        for number in [2, 5] {
            let nested = Fields::decode(entry.optional_bytes(number)?.unwrap_or_default())?;
            // The selected producer uses buffer field2; field1 is not a length gate.
            nested.optional_bytes(2)?;
        }
        if cgi == u32::from(command) {
            selected = CallHostPolicy::AxSelected;
        }
    }
    Ok(selected)
}

/// Successful protobuf decoding creates temporary copies of auth sections. Clear
/// those copies when this wrapper drops, including downstream validation errors.
/// Never include auth bytes in error text or diagnostics.
struct Fields(Vec<p::Field>);
impl Fields {
    fn decode(raw: &[u8]) -> Result<Self> {
        ensure!(raw.len() <= MAX_AUTH_BYTES, "Call auth snapshot too large");
        let result = Self(p::decode(raw)?);
        ensure!(result.0.len() <= MAX_ENTRIES, "Too many call auth fields");
        Ok(result)
    }
    fn optional_bytes(&self, number: u32) -> Result<Option<&[u8]>> {
        p::optional_data(&self.0, number)
    }
    fn required_bytes(&self, number: u32) -> Result<&[u8]> {
        self.optional_bytes(number)?
            .ok_or_else(|| anyhow::anyhow!("Missing call auth bytes field {number}"))
    }
    fn required_u64(&self, number: u32) -> Result<u64> {
        match p::optional(&self.0, number)? {
            Some(p::Value::Varint(value)) => Ok(*value),
            _ => anyhow::bail!("Missing or invalid call auth integer {number}"),
        }
    }
    fn optional_u32(&self, number: u32) -> Result<u32> {
        match p::optional(&self.0, number)? {
            None => Ok(0),
            Some(p::Value::Varint(value)) => Ok(u32::try_from(*value)?),
            _ => anyhow::bail!("Invalid call auth scalar {number}"),
        }
    }
}
impl Drop for Fields {
    fn drop(&mut self) {
        for field in &mut self.0 {
            if let p::Value::Bytes(bytes) = &mut field.value {
                bytes.zeroize();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(mask: u32, selected: u32) -> AuthSnapshot {
        let entry = p::message(&[p::uint(4, u64::from(selected))]).unwrap();
        let list = p::bytes(2, &entry);
        let mut snapshot = AuthSnapshot::default();
        snapshot.update_mask = mask;
        snapshot.response = p::message(&[
            p::bytes(1, &[8, 0]),
            p::uint(2, u64::from(mask)),
            p::bytes(6, &list),
        ])
        .unwrap();
        snapshot
    }
    #[test]
    fn active_exact_command_selects_ax_and_unrelated_command_does_not() {
        let state = snapshot(8, 7994);
        assert_eq!(
            selected_policy(&state, 7994).unwrap(),
            CallHostPolicy::AxSelected
        );
        assert_eq!(
            selected_policy(&state, 10342).unwrap(),
            CallHostPolicy::Ordinary
        );
    }
    #[test]
    fn inactive_retained_ax_section_cannot_select_a_route() {
        let mut state = snapshot(0, 7994);
        state.ax_auth = vec![0xff; 32];
        assert_eq!(
            selected_policy(&state, 7994).unwrap(),
            CallHostPolicy::Ordinary
        );
        state.update_mask = 8;
        assert!(selected_policy(&state, 7994).is_err());
    }
    #[test]
    fn missing_rejected_or_malformed_snapshot_never_defaults_to_ordinary() {
        assert!(selected_policy(&AuthSnapshot::default(), 7994).is_err());
        let mut state = snapshot(8, 7994);
        state.response = vec![10, 2, 8, 1, 16, 8];
        assert!(selected_policy(&state, 7994).is_err());
        state.response = vec![10, 2, 8, 0, 16, 8, 50, 2, 18, 0xff];
        assert!(selected_policy(&state, 7994).is_err());
        assert!(selected_policy(&snapshot(0, 0), 1).is_err());
    }

    #[test]
    fn expired_scope_transfer_exception_requires_matching_exit_frames() {
        fn request(delegate: u64, path: &[u8], task: u32) -> Vec<u8> {
            request_override(delegate, path, task, None)
        }
        fn request_override(
            delegate: u64,
            path: &[u8],
            task: u32,
            extra: Option<(u32, u64)>,
        ) -> Vec<u8> {
            let sdk = crate::protobuf::message(&[
                crate::protobuf::uint(1, 0x050000cb),
                crate::protobuf::bytes(2, b"0x1000000"),
            ])
            .unwrap();
            let group = crate::protobuf::message(&[
                crate::protobuf::bytes(1, b"wechat"),
                crate::protobuf::uint(2, 99),
                crate::protobuf::bytes(3, b"voip-2p"),
                crate::protobuf::bytes(4, b"group"),
                crate::protobuf::uint(5, 1),
                crate::protobuf::bytes(6, b""),
                crate::protobuf::uint(7, 0),
            ])
            .unwrap();
            let exit = crate::protobuf::message(&[
                crate::protobuf::bytes(1, &sdk),
                crate::protobuf::bytes(2, &group),
                crate::protobuf::uint(3, 7),
                crate::protobuf::uint(4, 9),
                crate::protobuf::uint(5, 0),
                crate::protobuf::uint(6, 1),
                crate::protobuf::uint(7, 0),
                crate::protobuf::uint(8, 1),
            ])
            .unwrap();
            let base = crate::protobuf::message(&[
                crate::protobuf::uint(1, 17),
                crate::protobuf::bytes(2, b"Ldevice"),
                crate::protobuf::uint(3, 1),
            ])
            .unwrap();
            let cloud = crate::protobuf::message(&[
                crate::protobuf::bytes(1, &base),
                crate::protobuf::bytes(2, &exit),
            ])
            .unwrap();
            let mut integers = std::collections::BTreeMap::from([
                (1, 1),
                (2, 17),
                (7, 0),
                (8, 0),
                (21, 0),
                (22, 17),
                (25, 0),
                (26, 4),
            ]);
            if let Some((field, value)) = extra {
                integers.insert(field, value);
            }
            let frame = super::super::app_frame::TaskFrame {
                command: task,
                integers,
                strings: std::collections::BTreeMap::from([
                    (24, b"wechat".to_vec()),
                    (27, b"server".to_vec()),
                ]),
                payload: super::super::app_frame::lz4_literal(&cloud).unwrap(),
            }
            .encode()
            .unwrap();
            let body = crate::protobuf::message(&[
                crate::protobuf::uint(1, 1901),
                crate::protobuf::uint(2, delegate),
                crate::protobuf::bytes(3, b"wechat"),
                crate::protobuf::bytes(4, path),
                crate::protobuf::bytes(5, &frame),
            ])
            .unwrap();
            super::super::signaling::CommonRequest {
                base_request: Some(vec![]),
                body: Some(body),
                unknown_fields: vec![],
            }
            .encode()
            .unwrap()
        }
        let exit = super::super::signaling::CallOperation::Exit
            .path()
            .as_bytes();
        let join = super::super::signaling::CallOperation::Join
            .path()
            .as_bytes();
        assert!(is_teardown_request(CallHostRoute::Transfer, &request(3380, exit, 3380)).unwrap());
        for raw in [
            request(3361, join, 3361),
            request(3380, join, 3380),
            request(3380, exit, 3361),
            request(3361, exit, 3380),
        ] {
            assert!(!is_teardown_request(CallHostRoute::Transfer, &raw).unwrap_or(false));
        }
        for extra in [(99, 0), (7, 11), (26, 0)] {
            assert!(!is_teardown_request(
                CallHostRoute::Transfer,
                &request_override(3380, exit, 3380, Some(extra))
            )
            .unwrap_or(false));
        }
        assert!(!is_teardown_request(CallHostRoute::AppAuth, &[]).unwrap());
        assert!(is_teardown_request(CallHostRoute::Transfer, &[0xff]).is_err());
    }
}
