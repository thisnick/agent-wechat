//! Standard SQLite WAL checks, independent of SQLite's permissive recovery scan.
use super::{Family, Member};
use anyhow::{ensure, Result};
use std::io::{Read, Seek, SeekFrom};

fn bytes(member: &Member, offset: u64, length: usize) -> Result<Vec<u8>> {
    let mut file = member.file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    let mut result = vec![0; length];
    file.read_exact(&mut result)?;
    Ok(result)
}
fn be(data: &[u8]) -> u32 {
    u32::from_be_bytes(data[..4].try_into().unwrap())
}
fn native(data: &[u8]) -> u32 {
    u32::from_ne_bytes(data[..4].try_into().unwrap())
}
fn checksum(data: &[u8], big: bool, mut sum: [u32; 2]) -> [u32; 2] {
    for pair in data.chunks_exact(8) {
        let word = |slice: &[u8]| {
            if big {
                be(slice)
            } else {
                u32::from_le_bytes(slice[..4].try_into().unwrap())
            }
        };
        sum[0] = sum[0].wrapping_add(word(pair)).wrapping_add(sum[1]);
        sum[1] = sum[1].wrapping_add(word(&pair[4..])).wrapping_add(sum[0]);
    }
    sum
}
struct Published {
    frame: u32,
    pages: u32,
    checksum: [u32; 2],
    salt: Vec<u8>,
    page_size: u32,
    big: bool,
}
fn index(member: &Member) -> Result<Option<Published>> {
    ensure!(
        member.length >= 32768 && member.length.is_multiple_of(32768),
        "unrecognized database WAL index; existing files preserved"
    );
    let data = bytes(member, 0, 96)?;
    if data.iter().all(|byte| *byte == 0) {
        return Ok(None);
    }
    ensure!(
        data[..48] == data[48..96] && native(&data) == 3007000 && data[12] == 1 && data[13] <= 1,
        "unrecognized database WAL index; existing files preserved"
    );
    ensure!(
        checksum(&data[..40], cfg!(target_endian = "big"), [0, 0])
            == [native(&data[40..]), native(&data[44..])],
        "invalid database WAL index checksum; existing files preserved"
    );
    let size = u16::from_ne_bytes(data[14..16].try_into().unwrap());
    Ok(Some(Published {
        frame: native(&data[16..]),
        pages: native(&data[20..]),
        checksum: [native(&data[24..]), native(&data[28..])],
        salt: data[32..40].to_vec(),
        page_size: if size == 1 { 65536 } else { u32::from(size) },
        big: data[13] != 0,
    }))
}

/// True requests a durable recovery family for unverified rollback recovery or
/// an uninitialized shared index. No source SQLite connection has opened yet.
pub(super) fn validate(family: &Family) -> Result<bool> {
    let mut retain = false;
    if let Some(journal) = family.0.get("-journal").filter(|m| m.length > 0) {
        // A copied super-journal can name files outside the copy. Store writes
        // never ATTACH databases, so preserve/refuse that unrecognized form.
        if journal.length >= 16 {
            let tail = bytes(journal, journal.length - 16, 16)?;
            ensure!(
                &tail[8..] != b"\xd9\xd5\x05\xf9\x20\xa1\x63\xd7" || be(&tail) == 0,
                "unsupported database super-journal; existing files preserved"
            );
        }
        retain = true;
    }
    let shm = family.0.get("-shm").filter(|m| m.length > 0);
    let published = shm.map(index).transpose()?.flatten();
    if shm.is_some() && published.is_none() {
        retain = true;
    }
    let Some(wal) = family.0.get("-wal").filter(|m| m.length > 0) else {
        ensure!(
            published.as_ref().is_none_or(|i| i.frame == 0),
            "database WAL index references a missing log; existing files preserved"
        );
        return Ok(retain);
    };
    ensure!(
        wal.length >= 32,
        "unrecognized database WAL; existing files preserved"
    );
    let header = bytes(wal, 0, 32)?;
    let magic = be(&header);
    ensure!(
        matches!(magic, 0x377f0682 | 0x377f0683) && be(&header[4..]) == 3007000,
        "unrecognized database WAL; existing files preserved"
    );
    let big = magic & 1 != 0;
    let page = be(&header[8..]);
    ensure!(
        (512..=65536).contains(&page) && page.is_power_of_two(),
        "invalid database WAL page size; existing files preserved"
    );
    let mut sum = checksum(&header[..24], big, [0, 0]);
    ensure!(
        sum == [be(&header[24..]), be(&header[28..])],
        "invalid database WAL checksum; existing files preserved"
    );
    let size = u64::from(page) + 24;
    let physical = (wal.length - 32) / size;
    let bound = published
        .as_ref()
        .map_or(physical, |index| u64::from(index.frame));
    ensure!(
        bound <= physical,
        "database WAL index exceeds log; existing files preserved"
    );
    if let Some(index) = &published {
        ensure!(
            index.page_size == page && index.big == big && index.salt == header[16..24],
            "database WAL index generation mismatch; existing files preserved"
        );
    } else {
        ensure!(
            (wal.length - 32) % size == 0,
            "incomplete database WAL without published index; existing files preserved"
        );
    }
    let mut commit = None;
    for number in 1..=bound {
        let frame = bytes(wal, 32 + (number - 1) * size, size as usize)?;
        ensure!(
            frame[8..16] == header[16..24] && be(&frame) > 0,
            "invalid database WAL frame identity; existing files preserved"
        );
        sum = checksum(&frame[..8], big, sum);
        sum = checksum(&frame[24..], big, sum);
        ensure!(
            sum == [be(&frame[16..]), be(&frame[20..])],
            "invalid database WAL frame checksum; existing files preserved"
        );
        let pages = be(&frame[4..]);
        if pages > 0 {
            commit = Some((number, pages, sum));
        }
    }
    if let Some(index) = published {
        if index.frame > 0 {
            ensure!(
                commit == Some((u64::from(index.frame), index.pages, index.checksum)),
                "database WAL published frame is not a verified commit; existing files preserved"
            );
        } else {
            // A zero published cursor cannot silently discard a committed log.
            ensure!(
                physical == 0,
                "database WAL has frames beyond an empty index; existing files preserved"
            );
        }
    } else if physical > 0 {
        ensure!(
            commit.is_some(),
            "database WAL has no verified commit; existing files preserved"
        );
    }
    Ok(retain)
}
