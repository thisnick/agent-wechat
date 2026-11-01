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
}
