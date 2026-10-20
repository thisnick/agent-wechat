//! Explicit stopped, disposable-clone validation. Input files are private; only
//! aggregate outcomes are printed. Never use this helper on an active profile.
use wechat_profile::*;
fn run() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.len() != 3 {
        return Err(ProfileError::InvalidQuery);
    }
    let location = ProfileLocation::new(&a[0])?;
    let keys = KeyFile::load(&a[1])?;
    let input: IncomingText =
        serde_json::from_slice(&std::fs::read(&a[2])?).map_err(|_| ProfileError::InvalidQuery)?;
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "explicit-disposable-fixture-validation".into(),
    };
    let first = apply_incoming_text(&location, &keys, &input, &options)?;
    let retry = apply_incoming_text(&location, &keys, &input, &options)?;
    let read = mark_read_messages(
        &location,
        &keys,
        &input.peer,
        std::slice::from_ref(&first.source_id),
        &options,
    )?;
    let read_retry = mark_read_messages(
        &location,
        &keys,
        &input.peer,
        std::slice::from_ref(&first.source_id),
        &options,
    )?;
    let reader = ProfileReader::open(
        location,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )?;
    let page = reader.messages(&MessageQuery {
        chat: Some(input.peer),
        limit: Some(200),
        ..Default::default()
    })?;
    let found = page
        .items
        .iter()
        .find(|v| v.id == first.source_id)
        .ok_or(ProfileError::InvalidMetadata)?;
    let exact = found.content.text.as_deref() == Some(input.content.as_str())
        && found.source.text.as_deref() == Some(input.source_xml.as_str())
        && found.server_message_id.as_deref() == Some(input.server_message_id.as_str());
    if !exact || retry.message_inserted || read_retry.removed_unread_rows != 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    println!(
        "{}",
        serde_json::json!({"newTextInserted":first.message_inserted,"retryInsertedDuplicate":retry.message_inserted,"exactBodySourceAndServerIdentity":exact,"removedUnreadRows":read.removed_unread_rows,"readRetryRemovedRows":read_retry.removed_unread_rows,"unreadBefore":read.unread_count_before,"unreadAfter":read.unread_count_after,"crossDatabaseAtomic":first.cross_database_atomic,"networkUsed":false,"officialRuntimeUsed":false})
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
