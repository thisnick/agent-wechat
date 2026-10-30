//! Experimental offline call-signaling codecs.
//!
//! These types do not open connections, obtain credentials or negotiate media.
//! Encoding a packet does not establish permission or readiness to submit it.
//! Optional fields preserve absence versus explicit zero, false or empty values;
//! unknown fields survive. Local limits are memory budgets, not server limits.

use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};

/// Local budget for a complete encoded signaling message, including its body.
pub const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
const MAX_FIELDS: usize = 1024;

/// Fixed command/path pairs; these do not select audio versus video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOperation {
    Create,
    Join,
    Acknowledge,
    Reject,
    Cancel,
    Exit,
}

impl CallOperation {
    pub const fn command_id(self) -> u32 {
        match self {
            Self::Create => 3066,
            Self::Join => 3361,
            Self::Acknowledge => 3007,
            Self::Reject => 3095,
            Self::Cancel => 3310,
            Self::Exit => 3380,
        }
    }

    pub const fn path(self) -> &'static str {
        match self {
            Self::Create => "/cloudim/cloud-bin/cloudvoipmtcreate",
            Self::Join => "/cloudim/cloud-bin/cloudvoipmtjoin",
            Self::Acknowledge => "/cloudim/cloud-bin/cloudvoipmtack",
            Self::Reject => "/cloudim/cloud-bin/cloudvoipmtreject",
            Self::Cancel => "/cloudim/cloud-bin/cloudvoipmtcancel",
            Self::Exit => "/cloudim/cloud-bin/cloudvoipmtexit",
        }
    }

    pub fn from_parts(command_id: u32, path: &str) -> Result<Self> {
        for operation in [
            Self::Create,
            Self::Join,
            Self::Acknowledge,
            Self::Reject,
            Self::Cancel,
            Self::Exit,
        ] {
            if command_id == operation.command_id() && path == operation.path() {
                return Ok(operation);
            }
        }
        bail!("Unsupported call command/path pair")
    }
}

/// Conference-to-host callback envelope. Bodies can contain private data, so
/// this and the network envelopes intentionally do not implement `Debug`.
///
/// The host's [`CommonRequest`] contains its standard base request and the
/// callback's inner body, not this entire callback envelope.
#[derive(Default)]
pub struct CloudRequest {
    pub task_id: Option<u32>,
    pub body: Option<Vec<u8>>,
    pub command_id: Option<u32>,
    pub path: Option<String>,
    pub longpolling: Option<bool>,
    /// Retained value; this codec does not assume its unit.
    pub longpolling_timeout: Option<u32>,
    pub unknown_fields: Vec<Field>,
}

impl CloudRequest {
    pub fn for_operation(task_id: u32, operation: CallOperation, body: Vec<u8>) -> Self {
        Self {
            task_id: Some(task_id),
            body: Some(body),
            command_id: Some(operation.command_id()),
            path: Some(operation.path().to_owned()),
            ..Self::default()
        }
    }

    /// A partial envelope can decode but must not be dispatched without its
    /// complete, supported operation identity.
    pub fn operation(&self) -> Result<CallOperation> {
        CallOperation::from_parts(
            self.command_id
                .ok_or_else(|| anyhow::anyhow!("Missing call command"))?,
            self.path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing call path"))?,
        )
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut fields = Vec::new();
        append_u32(&mut fields, 1, self.task_id);
        append_data(&mut fields, 2, self.body.as_deref());
        append_u32(&mut fields, 3, self.command_id);
        append_data(&mut fields, 4, self.path.as_deref().map(str::as_bytes));
        append_u32(&mut fields, 5, self.longpolling.map(u32::from));
        append_u32(&mut fields, 6, self.longpolling_timeout);
        finish(fields, &self.unknown_fields, 6)
    }

    pub fn decode(input: &[u8]) -> Result<Self> {
        let fields = decode_fields(input)?;
        let value = Self {
            task_id: optional_u32(&fields, 1)?,
            body: p::optional_data(&fields, 2)?.map(Vec::from),
            command_id: optional_u32(&fields, 3)?,
            path: p::optional_data(&fields, 4)?
                .map(|value| String::from_utf8(value.to_vec()))
                .transpose()?,
            longpolling: match optional_u32(&fields, 5)? {
                None => None,
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => bail!("Invalid call boolean"),
            },
            longpolling_timeout: optional_u32(&fields, 6)?,
            unknown_fields: unknown_fields(fields, 6),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<()> {
        if let Some(body) = &self.body {
            ensure!(body.len() <= MAX_SIGNALING_BYTES, "Call body too large");
        }
        if let Some(path) = &self.path {
            ensure!(
                path.len() <= 128 && !path.contains('\0'),
                "Invalid call path"
            );
        }
        if let (Some(command), Some(path)) = (self.command_id, self.path.as_deref()) {
            CallOperation::from_parts(command, path)?;
        }
        Ok(())
    }
}

/// Standard base request plus an opaque inner operation body. The existing
/// [`crate::wire::base_request`] prepares the standard base message. Validation
/// checks wire kinds and widths, not credential or account suitability.
#[derive(Default)]
pub struct CommonRequest {
    pub base_request: Option<Vec<u8>>,
    pub body: Option<Vec<u8>>,
    pub unknown_fields: Vec<Field>,
}

impl CommonRequest {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if let Some(base) = &self.base_request {
            validate_base_request(base)?;
        }
        encode_common(
            self.base_request.as_deref(),
            self.body.as_deref(),
            &self.unknown_fields,
        )
    }

    pub fn decode(input: &[u8]) -> Result<Self> {
        let (base_request, body, unknown_fields) = decode_common(input)?;
        if let Some(base) = &base_request {
            validate_base_request(base)?;
        }
        Ok(Self {
            base_request,
            body,
            unknown_fields,
        })
    }
}

/// Standard base response plus an opaque inner response. Decoding does not
/// establish success: inspect the return code before admitting an inner reply.
#[derive(Default)]
pub struct CommonResponse {
    pub base_response: Option<Vec<u8>>,
    pub body: Option<Vec<u8>>,
    pub unknown_fields: Vec<Field>,
}

impl CommonResponse {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if let Some(base) = &self.base_response {
            validate_base_response(base)?;
        }
        encode_common(
            self.base_response.as_deref(),
            self.body.as_deref(),
            &self.unknown_fields,
        )
    }

    pub fn decode(input: &[u8]) -> Result<Self> {
        let (base_response, body, unknown_fields) = decode_common(input)?;
        if let Some(base) = &base_response {
            validate_base_response(base)?;
        }
        Ok(Self {
            base_response,
            body,
            unknown_fields,
        })
    }

    /// Preserve negative failures and reject an absent return code.
    pub fn return_code(&self) -> Result<i32> {
        let base = self
            .base_response
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing call base response"))?;
        optional_i32(&decode_fields(base)?, 1)?
            .ok_or_else(|| anyhow::anyhow!("Missing call return code"))
    }
}

fn validate_base_request(input: &[u8]) -> Result<()> {
    let fields = decode_fields(input)?;
    p::optional_data(&fields, 1)?;
    optional_u32(&fields, 2)?;
    p::optional_data(&fields, 3)?;
    optional_i32(&fields, 4)?;
    p::optional_data(&fields, 5)?;
    optional_u32(&fields, 6)?;
    Ok(())
}

fn validate_base_response(input: &[u8]) -> Result<()> {
    let fields = decode_fields(input)?;
    optional_i32(&fields, 1)?;
    if let Some(message) = p::optional_data(&fields, 2)? {
        let message = decode_fields(message)?;
        if let Some(text) = p::optional_data(&message, 1)? {
            std::str::from_utf8(text)?;
        }
    }
    Ok(())
}

pub(super) fn decode_fields(input: &[u8]) -> Result<Vec<Field>> {
    ensure!(
        input.len() <= MAX_SIGNALING_BYTES,
        "Call envelope too large"
    );
    let fields = p::decode(input)?;
    ensure!(fields.len() <= MAX_FIELDS, "Too many call fields");
    Ok(fields)
}

pub(super) fn optional_u32(fields: &[Field], number: u32) -> Result<Option<u32>> {
    match p::optional(fields, number)? {
        None => Ok(None),
        Some(Value::Varint(value)) => Ok(Some(u32::try_from(*value)?)),
        _ => bail!("Invalid call scalar field {number}"),
    }
}

fn optional_i32(fields: &[Field], number: u32) -> Result<Option<i32>> {
    match p::optional(fields, number)? {
        None => Ok(None),
        Some(Value::Varint(value)) => Ok(Some(i32::try_from(*value as i64)?)),
        _ => bail!("Invalid call signed scalar field {number}"),
    }
}

pub(super) fn unknown_fields(fields: Vec<Field>, last_known: u32) -> Vec<Field> {
    fields
        .into_iter()
        .filter(|field| field.number > last_known)
        .collect()
}

fn append_u32(output: &mut Vec<Vec<u8>>, number: u32, value: Option<u32>) {
    if let Some(value) = value {
        output.push(p::uint(number, u64::from(value)));
    }
}

fn append_data(output: &mut Vec<Vec<u8>>, number: u32, value: Option<&[u8]>) {
    if let Some(value) = value {
        output.push(p::bytes(number, value));
    }
}

fn finish(mut fields: Vec<Vec<u8>>, unknown: &[Field], last_known: u32) -> Result<Vec<u8>> {
    ensure!(
        fields.len() + unknown.len() <= MAX_FIELDS,
        "Too many call fields"
    );
    let mut total = fields.iter().try_fold(0usize, |total, field| {
        total
            .checked_add(field.len())
            .ok_or_else(|| anyhow::anyhow!("Call length overflow"))
    })?;
    ensure!(total <= MAX_SIGNALING_BYTES, "Call envelope too large");
    for field in unknown {
        ensure!(
            field.number > last_known && field.number < 1 << 29,
            "Unknown call field collides with a known or invalid field"
        );
        let wire = match &field.value {
            Value::Varint(_) => 0,
            Value::Fixed64(_) => 1,
            Value::Bytes(_) => 2,
            Value::Fixed32(_) => 5,
        };
        let mut encoded = p::varint(u64::from(field.number) * 8 + wire);
        let value_len = match &field.value {
            Value::Varint(value) => p::varint(*value).len(),
            Value::Fixed64(_) => 8,
            Value::Fixed32(_) => 4,
            Value::Bytes(value) => {
                ensure!(value.len() <= MAX_SIGNALING_BYTES, "Call field too large");
                p::varint(value.len() as u64).len() + value.len()
            }
        };
        total = total
            .checked_add(encoded.len() + value_len)
            .ok_or_else(|| anyhow::anyhow!("Call length overflow"))?;
        ensure!(total <= MAX_SIGNALING_BYTES, "Call envelope too large");
        match &field.value {
            Value::Varint(value) => encoded.extend(p::varint(*value)),
            Value::Fixed64(value) => encoded.extend(value),
            Value::Fixed32(value) => encoded.extend(value),
            Value::Bytes(value) => {
                encoded.extend(p::varint(value.len() as u64));
                encoded.extend(value);
            }
        }
        fields.push(encoded);
    }
    p::message(&fields)
}

fn encode_common(base: Option<&[u8]>, body: Option<&[u8]>, unknown: &[Field]) -> Result<Vec<u8>> {
    if let Some(body) = body {
        ensure!(body.len() <= MAX_SIGNALING_BYTES, "Call body too large");
    }
    let mut fields = Vec::new();
    append_data(&mut fields, 1, base);
    append_data(&mut fields, 2, body);
    finish(fields, unknown, 2)
}

type CommonParts = (Option<Vec<u8>>, Option<Vec<u8>>, Vec<Field>);
fn decode_common(input: &[u8]) -> Result<CommonParts> {
    let fields = decode_fields(input)?;
    let base = p::optional_data(&fields, 1)?.map(Vec::from);
    let body = p::optional_data(&fields, 2)?.map(Vec::from);
    Ok((base, body, unknown_fields(fields, 2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_has_independent_wire_vector() {
        // f1=7,f2=[8,1],f3=3066,f4=create-path,f5=false,f6=60000.
        let expected = hex::decode(concat!(
            "08071202080118fa172224",
            "2f636c6f7564696d2f636c6f75642d62696e2f636c6f7564766f69706d74637265617465",
            "280030e0d403"
        ))
        .unwrap();
        let request = CloudRequest {
            longpolling: Some(false),
            longpolling_timeout: Some(60_000),
            ..CloudRequest::for_operation(7, CallOperation::Create, vec![8, 1])
        };
        assert_eq!(request.encode().unwrap(), expected);
        let decoded = CloudRequest::decode(&expected).unwrap();
        assert_eq!(decoded.task_id, Some(7));
        assert_eq!(decoded.body.as_deref(), Some([8, 1].as_slice()));
        assert_eq!(decoded.operation().unwrap(), CallOperation::Create);
        assert_eq!(decoded.longpolling, Some(false));
        assert_eq!(decoded.longpolling_timeout, Some(60_000));
    }

    #[test]
    fn absence_and_present_defaults_are_distinct() {
        let absent = CloudRequest::decode(&[]).unwrap();
        assert!(absent.task_id.is_none() && absent.body.is_none() && absent.longpolling.is_none());
        assert!(absent.encode().unwrap().is_empty());
        assert!(absent.operation().is_err());
        let input = [8, 0, 18, 0, 40, 0, 48, 0];
        let present = CloudRequest::decode(&input).unwrap();
        assert_eq!(present.task_id, Some(0));
        assert_eq!(present.body.as_deref(), Some([].as_slice()));
        assert_eq!(present.longpolling, Some(false));
        assert_eq!(present.longpolling_timeout, Some(0));
        assert_eq!(present.encode().unwrap(), input);
    }

    #[test]
    fn commands_and_paths_must_match() {
        for operation in [
            CallOperation::Create,
            CallOperation::Join,
            CallOperation::Acknowledge,
            CallOperation::Reject,
            CallOperation::Cancel,
            CallOperation::Exit,
        ] {
            assert_eq!(
                CallOperation::from_parts(operation.command_id(), operation.path()).unwrap(),
                operation
            );
            assert!(
                CallOperation::from_parts(operation.command_id() + 1, operation.path()).is_err()
            );
        }
        assert!(CallOperation::from_parts(3066, CallOperation::Join.path()).is_err());
        assert!(CallOperation::from_parts(3066, "https://example.invalid").is_err());
        assert!(CallOperation::from_parts(3066, "/cloudim/cloud-bin/cloudvoipmtcreate/").is_err());
    }

    #[test]
    fn malformed_callback_fields_are_rejected() {
        for input in [
            vec![8, 1, 8, 1],
            vec![10, 0],
            vec![16, 0],
            vec![26, 0],
            vec![32, 0],
            vec![42, 0],
            vec![50, 0],
            vec![40, 2],
            vec![8, 128, 128, 128, 128, 16],
            vec![48, 128, 128, 128, 128, 16],
            vec![34, 1, 255],
            vec![18, 1],
            vec![0],
        ] {
            assert!(CloudRequest::decode(&input).is_err());
        }
        let mut request = CloudRequest::for_operation(7, CallOperation::Create, vec![]);
        request.path = Some(CallOperation::Join.path().to_owned());
        assert!(request.encode().is_err());
    }

    #[test]
    fn unknown_fields_preserve_wire_kinds() {
        let input = hex::decode("38004101020304050607084a0200ff5501020304").unwrap();
        let value = CloudRequest::decode(&input).unwrap();
        assert_eq!(value.unknown_fields.len(), 4);
        assert_eq!(value.encode().unwrap(), input);
        let mut value = CloudRequest::default();
        value.unknown_fields.push(Field {
            number: 1,
            value: Value::Varint(0),
        });
        assert!(value.encode().is_err());
    }

    #[test]
    fn common_envelopes_keep_base_and_opaque_body() {
        let input = [10, 0, 18, 2, 8, 1];
        let request = CommonRequest::decode(&input).unwrap();
        assert_eq!(request.base_request.as_deref(), Some([].as_slice()));
        assert_eq!(request.body.as_deref(), Some([8, 1].as_slice()));
        assert_eq!(request.encode().unwrap(), input);
        let response = CommonResponse::decode(&[10, 2, 8, 0, 18, 0]).unwrap();
        assert_eq!(response.return_code().unwrap(), 0);
        assert_eq!(response.encode().unwrap(), [10, 2, 8, 0, 18, 0]);
        assert!(CommonResponse::decode(&[]).unwrap().return_code().is_err());
        assert!(CommonResponse::decode(&[10, 0])
            .unwrap()
            .return_code()
            .is_err());
        let input = hex::decode("0a0b08ffffffffffffffffff0112020801").unwrap();
        let response = CommonResponse::decode(&input).unwrap();
        assert_eq!(response.return_code().unwrap(), -1);
        assert_eq!(response.encode().unwrap(), input);
    }

    #[test]
    fn nested_bases_and_duplicate_bodies_are_checked() {
        for input in [
            vec![8, 0],
            vec![10, 0, 10, 0],
            vec![18, 0, 18, 0],
            vec![10, 2, 8, 0],
            vec![10, 2, 18, 0],
        ] {
            assert!(CommonRequest::decode(&input).is_err());
        }
        for input in [
            vec![10, 2, 10, 0],
            vec![10, 2, 16, 0],
            vec![10, 4, 8, 0, 8, 0],
            vec![10, 4, 18, 2, 8, 0],
        ] {
            assert!(CommonResponse::decode(&input).is_err());
        }
    }

    #[test]
    fn size_budget_counts_headers_and_unknown_fields() {
        let request = CloudRequest {
            body: Some(vec![0; MAX_SIGNALING_BYTES]),
            ..CloudRequest::default()
        };
        assert!(request.encode().is_err());
        assert!(CloudRequest::decode(&vec![0; MAX_SIGNALING_BYTES + 1]).is_err());
        let mut request = CloudRequest::default();
        for _ in 0..=MAX_FIELDS {
            request.unknown_fields.push(Field {
                number: 7,
                value: Value::Varint(0),
            });
        }
        assert!(request.encode().is_err());
    }
}
