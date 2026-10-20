//! Bounded numbered protobuf primitives. Unknown fields and full u64 values survive.
use crate::MAX_MESSAGE_BYTES;
use anyhow::{bail, ensure, Result};

#[derive(Clone, PartialEq, Eq)]
pub enum Value {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(Vec<u8>),
    Fixed32([u8; 4]),
}
#[derive(Clone, PartialEq, Eq)]
pub struct Field {
    pub number: u32,
    pub value: Value,
}

pub fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (value & 127) as u8;
        value >>= 7;
        out.push(b | if value > 0 { 128 } else { 0 });
        if value == 0 {
            break;
        }
    }
    out
}
pub fn read_varint(input: &[u8], offset: &mut usize) -> Result<u64> {
    let mut out = 0u64;
    for i in 0..10 {
        let b = *input
            .get(*offset)
            .ok_or_else(|| anyhow::anyhow!("Truncated varint"))?;
        *offset += 1;
        ensure!(i != 9 || b <= 1, "Varint overflow");
        out |= ((b & 127) as u64) << (i * 7);
        if b & 128 == 0 {
            return Ok(out);
        }
    }
    bail!("Unterminated varint")
}
fn tag(number: u32, wire: u64) -> Vec<u8> {
    assert!(number > 0 && number < 1 << 29);
    varint((number as u64) * 8 + wire)
}
pub fn uint(number: u32, value: u64) -> Vec<u8> {
    [tag(number, 0), varint(value)].concat()
}
pub fn int32(number: u32, value: i32) -> Vec<u8> {
    uint(number, value as i64 as u64)
}
pub fn bytes(number: u32, value: &[u8]) -> Vec<u8> {
    [tag(number, 2), varint(value.len() as u64), value.to_vec()].concat()
}
pub fn message(parts: &[Vec<u8>]) -> Result<Vec<u8>> {
    let size = parts.iter().try_fold(0usize, |n, p| {
        n.checked_add(p.len())
            .ok_or_else(|| anyhow::anyhow!("Message length overflow"))
    })?;
    ensure!(size <= MAX_MESSAGE_BYTES, "Message too large");
    Ok(parts.concat())
}
pub fn buffer(value: &[u8]) -> Vec<u8> {
    [uint(1, value.len() as u64), bytes(2, value)].concat()
}
pub fn decode(input: &[u8]) -> Result<Vec<Field>> {
    ensure!(input.len() <= MAX_MESSAGE_BYTES, "Message too large");
    let mut offset = 0;
    let mut out = Vec::new();
    while offset < input.len() {
        ensure!(out.len() < 65536, "Too many fields");
        let key = read_varint(input, &mut offset)?;
        let number = key >> 3;
        ensure!(number > 0 && number < 1 << 29, "Invalid field tag");
        let value = match key & 7 {
            0 => Value::Varint(read_varint(input, &mut offset)?),
            wire @ (1 | 2 | 5) => {
                let n = match wire {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(read_varint(input, &mut offset)?)?,
                };
                ensure!(
                    n <= MAX_MESSAGE_BYTES && n <= input.len() - offset,
                    "Truncated field"
                );
                let value = &input[offset..offset + n];
                offset += n;
                match wire {
                    1 => Value::Fixed64(value.try_into()?),
                    5 => Value::Fixed32(value.try_into()?),
                    _ => Value::Bytes(value.to_vec()),
                }
            }
            _ => bail!("Unsupported wire type"),
        };
        out.push(Field {
            number: number as u32,
            value,
        });
    }
    Ok(out)
}
pub fn optional(fields: &[Field], number: u32) -> Result<Option<&Value>> {
    let mut found = fields.iter().filter(|f| f.number == number);
    let first = found.next();
    ensure!(found.next().is_none(), "Ambiguous field {number}");
    Ok(first.map(|f| &f.value))
}
pub fn scalar(fields: &[Field], number: u32) -> Result<u64> {
    match optional(fields, number)? {
        Some(Value::Varint(v)) => Ok(*v),
        _ => bail!("Missing scalar field {number}"),
    }
}
pub fn data(fields: &[Field], number: u32) -> Result<&[u8]> {
    match optional(fields, number)? {
        Some(Value::Bytes(v)) => Ok(v),
        _ => bail!("Missing bytes field {number}"),
    }
}
pub fn optional_data(fields: &[Field], number: u32) -> Result<Option<&[u8]>> {
    match optional(fields, number)? {
        Some(Value::Bytes(v)) => Ok(Some(v)),
        None => Ok(None),
        _ => bail!("Invalid bytes field {number}"),
    }
}
pub fn unbuffer(input: &[u8]) -> Result<Vec<u8>> {
    let f = decode(input)?;
    let raw = optional_data(&f, 2)?.unwrap_or_default();
    ensure!(scalar(&f, 1)? == raw.len() as u64, "Buffer length mismatch");
    Ok(raw.to_vec())
}
pub fn base_code(fields: &[Field]) -> Result<i32> {
    let base = decode(data(fields, 1)?)?;
    Ok(scalar(&base, 1)? as u32 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn u64_and_unknowns() {
        let input = message(&[uint(1, u64::MAX), bytes(900, b"unknown"), uint(1, 7)]).unwrap();
        let f = decode(&input).unwrap();
        assert_eq!(f.len(), 3);
        assert!(scalar(&f, 1).is_err());
        assert!(matches!(f[0].value, Value::Varint(u64::MAX)));
    }
    #[test]
    fn malformed_rejected() {
        for x in [
            vec![0],
            vec![8, 255],
            vec![10, 3, 0],
            vec![8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2],
            vec![11],
        ] {
            assert!(decode(&x).is_err());
        }
    }
}
