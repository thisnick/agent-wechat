use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessContactProjection {
    pub service_type: i32,
    pub accept_type: i32,
    pub child_type: i32,
    pub brand_flag: u32,
    pub home_url: String,
    pub external_info: String,
    pub brand_info: String,
    pub brand_icon_url: String,
    pub brand_list: String,
    pub belong: String,
}

/// Apply a received business contact and its ordinary extra metadata in one
/// transaction. This performs no refresh or other account operation.
pub fn apply_contact_business_patch(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &ContactProjection,
    business: &BusinessContactProjection,
    options: &WriteOptions,
) -> Result<AppliedContact> {
    ownership(location, keys, options)?;
    identity(&input.username)?;
    if input.username.contains('@')
        || input.fields.contains_key("id")
        || input.fields.contains_key("username")
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let database = find_database(location, "contact.db")?;
    let mut connection = open_write(location, &database, keys, options.allow_plaintext)?;
    for table in ["name2id", "contact", "biz_info"] {
        check_known_table(&connection, table)?;
    }
    let transaction = connection.transaction()?;
    let id = map_id(&transaction, "name2id", "username", &input.username)?;
    let existing = checked_identity(&transaction, "contact", id, &input.username)?;
    checked_identity(&transaction, "biz_info", id, &input.username)?;
    let previous = if existing {
        transaction.query_row(
            "SELECT local_type,verify_flag,extra_buffer FROM contact WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )?
    } else {
        (None, None, None)
    };
    let effective_integer = |column: &str, previous: Option<i64>| -> Result<Option<i64>> {
        input.fields.get(column).map_or(Ok(previous), |value| {
            if let SqlValue::Integer(value) = value {
                value
                    .parse()
                    .map(Some)
                    .map_err(|_| ProfileError::InvalidMetadata)
            } else {
                Err(ProfileError::InvalidMetadata)
            }
        })
    };
    if !matches!(effective_integer("local_type", previous.0)?, Some(0..=4))
        || effective_integer("verify_flag", previous.1)?
            .and_then(|value| u32::try_from(value).ok())
            .is_none_or(|value| value & 8 == 0)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let extra = match input.fields.get("extra_buffer") {
        None => previous.2.unwrap_or_default(),
        Some(SqlValue::Null) => Vec::new(),
        Some(SqlValue::Blob(value)) => STANDARD
            .decode(value)
            .map_err(|_| ProfileError::InvalidMetadata)?,
        _ => return Err(ProfileError::InvalidMetadata),
    };
    let extra = replace_service_type(&extra, business.service_type)?;
    let mut contact_fields = if existing {
        RawFields::new()
    } else {
        nullable_fields(&transaction, &database, "contact", &["id", "username"])?
    };
    contact_fields.extend(input.fields.clone());
    contact_fields.insert("username".into(), SqlValue::Text(input.username.clone()));
    contact_fields.insert(
        "extra_buffer".into(),
        SqlValue::Blob(STANDARD.encode(extra)),
    );
    let contact_changed = apply_row(
        &transaction,
        "contact",
        "id",
        SqlValue::Integer(id.to_string()),
        &contact_fields,
        true,
    )?;
    let business_fields = business_fields(&input.username, business);
    let business_changed = apply_row(
        &transaction,
        "biz_info",
        "id",
        SqlValue::Integer(id.to_string()),
        &business_fields,
        true,
    )?;
    transaction.commit()?;
    Ok(AppliedContact {
        local_id: id,
        changed: contact_changed || business_changed,
    })
}

fn checked_identity(connection: &Connection, table: &str, id: i64, username: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!(
        "SELECT id,username FROM {} WHERE id=?1 OR username=?2 LIMIT 3",
        quote_identifier(table)
    ))?;
    let rows = statement
        .query_map(params![id, username], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match rows.as_slice() {
        [] => Ok(false),
        [(stored_id, stored_name)] if *stored_id == id && stored_name == username => Ok(true),
        _ => Err(ProfileError::InvalidMetadata),
    }
}

fn replace_service_type(extra: &[u8], service_type: i32) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    for chunk in proto_chunks(extra)? {
        if chunk.field == 13 {
            if chunk.wire != 0 {
                return Err(ProfileError::InvalidMetadata);
            }
        } else {
            output.extend_from_slice(chunk.raw);
        }
    }
    var_out(13 << 3, &mut output);
    var_out(service_type as i64 as u64, &mut output);
    if output.len() > 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    Ok(output)
}

fn business_fields(username: &str, business: &BusinessContactProjection) -> RawFields {
    let mut fields = RawFields::new();
    fields.insert("username".into(), SqlValue::Text(username.to_owned()));
    for (name, value) in [
        ("type", i64::from(business.service_type)),
        ("accept_type", i64::from(business.accept_type)),
        ("child_type", i64::from(business.child_type)),
        ("brand_flag", i64::from(business.brand_flag)),
    ] {
        fields.insert(name.into(), SqlValue::Integer(value.to_string()));
    }
    for (name, value) in [
        ("home_url", &business.home_url),
        ("external_info", &business.external_info),
        ("brand_info", &business.brand_info),
        ("brand_icon_url", &business.brand_icon_url),
        ("brand_list", &business.brand_list),
        ("belong", &business.belong),
    ] {
        fields.insert(name.into(), SqlValue::Text(value.clone()));
    }
    fields.insert("version".into(), SqlValue::Integer("0".into()));
    fields.insert("sync_version".into(), SqlValue::Text(String::new()));
    fields.insert("ext_buffer".into(), SqlValue::Blob(STANDARD.encode([])));
    fields
}

#[cfg(test)]
#[path = "business_projection/tests.rs"]
mod tests;
