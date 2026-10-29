//! Read only after the operator has stopped the writer or made a consistent copy.
use wechat_profile::{KeyFile, ListQuery, ProfileLocation, ProfileReader, ReadOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 || args[0] != "--stopped-or-consistent-copy" {
        return Err("usage: contacts --stopped-or-consistent-copy ACCOUNT_ROOT KEYS_FILE".into());
    }
    let location = ProfileLocation::new(&args[1])?;
    let keys = KeyFile::load(&args[2])?;
    let reader = ProfileReader::open(
        location,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )?;
    println!(
        "{} contacts",
        reader.contacts(&ListQuery::default())?.items.len()
    );
    Ok(())
}
