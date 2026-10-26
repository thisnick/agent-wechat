use super::*;

struct PreviewRoom {
    name: String,
    displays: BTreeMap<String, Option<String>>,
    first: Vec<String>,
    last: Option<String>,
    count: usize,
    valid: bool,
}

impl ProfileReader {
    /// Derive notification preferences for a page without refreshing contacts.
    /// Direct contacts and groups use different official fields; incomplete
    /// identities and unexpected values remain unknown.
    pub fn thread_muted(&self, threads: &[String]) -> Result<BTreeMap<String, Option<bool>>> {
        if threads.len() > 200 {
            return Err(ProfileError::InvalidQuery);
        }
        if threads.is_empty() {
            return Ok(BTreeMap::new());
        }
        for thread in threads {
            validate_name(thread)?;
        }
        let c = self.snapshot.open(self.role("contact.db", "contacts")?)?;
        #[cfg(test)]
        super::batch_lookup_tests::record_batch_query();
        let placeholders = vec!["?"; threads.len()].join(",");
        let mut statement = c.prepare(&format!(
            "SELECT username,flag,chat_room_notify,local_type,delete_flag FROM contact WHERE username IN ({placeholders})"
        ))?;
        let rows = statement.query_map(rusqlite::params_from_iter(threads), raw)?;
        let mut muted = BTreeMap::new();
        for row in rows {
            let fields = row?;
            let name = text(&fields, "username").ok_or(ProfileError::InvalidMetadata)?;
            let value = if name.ends_with("@chatroom") {
                match num(&fields, "chat_room_notify") {
                    Some(0) => Some(true),
                    Some(1) => Some(false),
                    _ => None,
                }
            } else if num(&fields, "local_type") == Some(1)
                && num(&fields, "delete_flag") == Some(0)
            {
                num(&fields, "flag")
                    .and_then(|flag| u32::try_from(flag).ok())
                    .filter(|flag| flag & 1 != 0)
                    .map(|flag| flag & 0x200 != 0)
            } else {
                None
            };
            muted.insert(name, value);
        }
        Ok(muted)
    }

    /// Read avatar members for a page of groups with three batched queries.
    /// Validate membership against the same official room metadata used by
    /// group_members; an inconsistent room does not hide other valid rooms.
    pub fn group_avatar_members(
        &self,
        rooms: &[String],
    ) -> Result<BTreeMap<String, Vec<GroupMember>>> {
        if rooms.len() > 200 {
            return Err(ProfileError::InvalidQuery);
        }
        if rooms.is_empty() {
            return Ok(BTreeMap::new());
        }
        for room in rooms {
            validate_name(room)?;
        }
        let c = self.snapshot.open(self.role("contact.db", "groups")?)?;
        let placeholders = vec!["?"; rooms.len()].join(",");
        let mut statement = c.prepare(&format!(
            "SELECT id,username,ext_buffer FROM chat_room WHERE username IN ({placeholders})"
        ))?;
        let rows = statement.query_map(rusqlite::params_from_iter(rooms), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })?;
        let mut selected = BTreeMap::new();
        for row in rows {
            let (id, name, bytes) = row?;
            let Ok(displays) = room_displays(bytes.as_deref().unwrap_or_default()) else {
                continue;
            };
            selected.insert(
                id,
                PreviewRoom {
                    name,
                    displays,
                    first: Vec::new(),
                    last: None,
                    count: 0,
                    valid: true,
                },
            );
        }
        if selected.is_empty() {
            return Ok(BTreeMap::new());
        }
        let placeholders = vec!["?"; selected.len()].join(",");
        let ids: Vec<_> = selected.keys().copied().collect();
        let mut statement = c.prepare(&format!("SELECT m.room_id,c.username FROM chatroom_member m JOIN contact c ON c.id=m.member_id WHERE m.room_id IN ({placeholders}) ORDER BY m.room_id,c.username"))?;
        let rows = statement.query_map(rusqlite::params_from_iter(ids), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, username) = row?;
            let room = selected.get_mut(&id).ok_or(ProfileError::InvalidMetadata)?;
            if room.last.as_ref() == Some(&username) {
                continue;
            }
            room.last = Some(username.clone());
            room.count += 1;
            if room.count > 100_000 {
                return Err(ProfileError::ResourceLimit);
            }
            if !room.displays.is_empty() && !room.displays.contains_key(&username) {
                room.valid = false
            }
            if room.first.len() < 4 {
                room.first.push(username)
            }
        }
        selected.retain(|_, room| {
            room.valid && (room.displays.is_empty() || room.displays.len() == room.count)
        });
        let names: BTreeSet<_> = selected
            .values()
            .flat_map(|room| room.first.iter())
            .collect();
        let mut contacts = BTreeMap::new();
        if !names.is_empty() {
            let placeholders = vec!["?"; names.len()].join(",");
            let mut statement = c.prepare(&format!(
                "SELECT * FROM contact WHERE username IN ({placeholders})"
            ))?;
            let rows = statement.query_map(rusqlite::params_from_iter(names), raw)?;
            for row in rows {
                let value = contact(row?)?;
                contacts.insert(value.username.clone(), value);
            }
        }
        Ok(selected
            .into_values()
            .map(|room| {
                let members = room
                    .first
                    .into_iter()
                    .filter_map(|name| {
                        contacts.get(&name).cloned().map(|contact| {
                            group_member(contact, room.displays.get(&name).cloned().flatten())
                        })
                    })
                    .collect();
                (room.name, members)
            })
            .collect())
    }
}
