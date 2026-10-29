//! A local codec example; no network requests or account credentials.
use wechat_protocol::protobuf::{decode, message, scalar, uint};

fn main() -> anyhow::Result<()> {
    let encoded = message(&[uint(1, 42)])?;
    let fields = decode(&encoded)?;
    println!("{}", scalar(&fields, 1)?);
    Ok(())
}
