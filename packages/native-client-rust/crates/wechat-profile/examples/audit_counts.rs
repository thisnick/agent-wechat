//! Explicit offline validation helper. Prints aggregate counts, never identities,
//! message bodies, key values, cache URLs or source paths.
use wechat_profile::*;
fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or(ProfileError::InvalidQuery)?;
    let keys = args.next();
    let location = ProfileLocation::new(path)?;
    let (keys, plaintext) = if let Some(path) = keys {
        (KeyFile::load(path)?, false)
    } else {
        (KeyFile::empty(location.account_directory()), true)
    };
    let r = ProfileReader::open(
        location,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: plaintext,
            ..Default::default()
        },
    )?;
    let mut contacts = 0;
    let mut avatars = 0;
    let mut q = ListQuery {
        limit: Some(200),
        ..Default::default()
    };
    loop {
        let p = r.contacts(&q)?;
        for c in &p.items {
            if r.avatar(&c.username).is_ok() {
                avatars += 1;
            }
        }
        contacts += p.items.len();
        q.cursor = p.next_cursor;
        if q.cursor.is_none() {
            break;
        }
    }
    let mut threads = 0;
    let mut q = ListQuery {
        limit: Some(200),
        ..Default::default()
    };
    loop {
        let p = r.threads(&q)?;
        threads += p.items.len();
        q.cursor = p.next_cursor;
        if q.cursor.is_none() {
            break;
        }
    }
    let mut groups = 0;
    let mut members = 0;
    let mut q = ListQuery {
        limit: Some(200),
        ..Default::default()
    };
    loop {
        let p = r.groups(&q)?;
        for g in &p.items {
            let mut mq = ListQuery {
                limit: Some(200),
                ..Default::default()
            };
            loop {
                let mp = r.group_members(&g.username, &mq)?;
                members += mp.items.len();
                mq.cursor = mp.next_cursor;
                if mq.cursor.is_none() {
                    break;
                }
            }
        }
        groups += p.items.len();
        q.cursor = p.next_cursor;
        if q.cursor.is_none() {
            break;
        }
    }
    let mut messages = 0;
    let mut unresolved = 0;
    let mut decoded = 0;
    let mut q = MessageQuery {
        limit: Some(200),
        ..Default::default()
    };
    loop {
        let p = r.messages(&q)?;
        for m in &p.items {
            unresolved += usize::from(m.peer.is_none());
            decoded += usize::from(m.content.text.is_some());
        }
        messages += p.items.len();
        q.cursor = p.next_cursor;
        if q.cursor.is_none() {
            break;
        }
    }
    println!(
        "{}",
        serde_json::json!({"databases":r.inventory().len(),"partial":r.coverage().partial,"contacts":contacts,"threads":threads,"groups":groups,"groupMembers":members,"cachedAvatars":avatars,"messages":messages,"decodedBodies":decoded,"unresolvedMessages":unresolved,"writesPerformed":false})
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
