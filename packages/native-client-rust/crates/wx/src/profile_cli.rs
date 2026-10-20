use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use wechat_profile::{
    ImageQuality, KeyFile, ListQuery, MessageQuery, ProfileLocation, ProfileReader, ReadOptions,
    SourceMessageId,
};

#[derive(clap::Args)]
pub struct ReadFlags {
    /// Confirm that the selected source is stopped or is a consistent snapshot.
    #[arg(long)]
    pub source_stopped: bool,
    /// Permit a deliberately created plaintext export; encrypted sources remain the default.
    #[arg(long)]
    pub allow_plaintext: bool,
    #[arg(long,default_value_t=50,value_parser=clap::value_parser!(u16).range(1..=200))]
    pub limit: u16,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub search: Option<String>,
    /// Include original typed source fields, which can contain private transport metadata.
    #[arg(long)]
    pub raw: bool,
}
#[derive(Subcommand)]
pub enum ProfileCommand {
    Add {
        name: String,
        #[arg(long)]
        path: PathBuf,
        #[arg(long, default_value = "official-linux")]
        format: String,
        #[arg(long)]
        account_dir: Option<String>,
        #[arg(long)]
        keys_file: Option<PathBuf>,
    },
    List,
    Use {
        name: String,
    },
    Inspect,
    Chats {
        #[command(flatten)]
        read: ReadFlags,
    },
    Messages {
        #[arg(long)]
        chat: String,
        #[arg(long)]
        own_username: Option<String>,
        #[command(flatten)]
        read: ReadFlags,
    },
    Contacts {
        #[command(flatten)]
        read: ReadFlags,
    },
    Groups {
        #[arg(long,conflicts_with_all=["search","cursor"])]
        id: Option<String>,
        #[arg(long, requires = "id")]
        members: bool,
        #[command(flatten)]
        read: ReadFlags,
    },
    Images {
        #[arg(long)]
        message: String,
        #[arg(long,default_value="best",value_parser=["best","thumbnail","standard","original","full"])]
        quality: String,
        #[arg(long)]
        output: PathBuf,
        #[command(flatten)]
        read: ReadFlags,
    },
    Avatars {
        #[arg(long)]
        username: String,
        #[arg(long)]
        output: PathBuf,
        #[command(flatten)]
        read: ReadFlags,
    },
    Keys {
        #[command(subcommand)]
        command: KeyCommand,
    },
}
#[derive(Subcommand)]
pub enum KeyCommand {
    Export {
        name: Option<String>,
        #[arg(long, conflicts_with = "from_agent_data")]
        password_file: Option<PathBuf>,
        #[arg(long, conflicts_with = "password_file")]
        from_agent_data: Option<PathBuf>,
        #[arg(long, requires = "from_agent_data")]
        agent_token_file: Option<PathBuf>,
        #[arg(long, requires = "from_agent_data")]
        session: Option<String>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, required = true)]
        source_stopped: bool,
    },
}
fn options(read: &ReadFlags) -> ReadOptions {
    ReadOptions {
        stopped_or_consistent_snapshot: read.source_stopped,
        allow_plaintext: read.allow_plaintext,
        ..Default::default()
    }
}
fn source(profile: &settings::LocalProfile) -> Result<ProfileLocation> {
    if profile.format != "official-linux" {
        bail!("unsupported_profile_version: this reader supports official-linux");
    }
    let path = if let Some(account) = &profile.account_dir {
        if Path::new(account).components().count() != 1 || account == "." || account == ".." {
            bail!("invalid account directory");
        }
        if profile.path.join("xwechat_files").is_dir() {
            profile.path.join("xwechat_files").join(account)
        } else {
            profile.path.join(account)
        }
    } else {
        profile.path.clone()
    };
    ProfileLocation::new(path)
        .context("select one account root containing db_storage, or set --account-dir")
}
fn reader(profile: &settings::LocalProfile, read: &ReadFlags) -> Result<ProfileReader> {
    let location = source(profile)?;
    let keys = match &profile.keys_file {
        Some(file) => KeyFile::load(file)?,
        None if read.allow_plaintext => KeyFile::empty(location.account_directory()),
        None => bail!("keys_missing: configure a protected keys file or explicitly export one"),
    };
    Ok(ProfileReader::open(location, keys, options(read))?)
}
fn destination(
    profile: &settings::LocalProfile,
    location: &ProfileLocation,
    path: &Path,
) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = std::fs::canonicalize(parent)?;
    let profile_root = std::fs::canonicalize(&profile.path)?;
    if parent.starts_with(location.root()) || parent.starts_with(profile_root) {
        bail!("output must be outside the source profile");
    }
    Ok(())
}
fn safe(mut value: Value, raw: bool) -> Value {
    if !raw {
        match &mut value {
            Value::Object(map) => {
                map.remove("rawFields");
                map.remove("source");
                if map
                    .get("messageType")
                    .and_then(Value::as_i64)
                    .is_some_and(|t| t != 1)
                {
                    map.insert("content".into(), json!({"text":null,"state":"typed_media"}));
                }
                for child in map.values_mut() {
                    *child = safe(child.take(), false);
                }
            }
            Value::Array(items) => {
                for child in items {
                    *child = safe(child.take(), false);
                }
            }
            _ => {}
        }
    }
    value
}
fn wrap<T: serde::Serialize>(
    name: &str,
    reader: &ProfileReader,
    page: &wechat_profile::Page<T>,
    raw: bool,
) -> Result<Value> {
    let mut value = safe(serde_json::to_value(page)?, raw);
    value["format"] = json!("official-linux");
    value["adapterVersion"] = json!(1);
    value["localProfile"] = json!(name);
    value["sourceView"] = json!(reader.generation());
    Ok(value)
}
fn handle(name: &str, generation: &str, id: &SourceMessageId) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(
        &json!({"v":1,"profile":name,"view":generation,"source":id}),
    )?))
}
fn parse_handle(input: &str, name: &str, generation: &str) -> Result<SourceMessageId> {
    if input.len() > 8192 {
        bail!("invalid observation ID");
    }
    let value: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(input)
            .context("invalid observation ID")?,
    )?;
    if value["v"] != 1 || value["profile"] != name {
        bail!("invalid observation binding");
    }
    if value["view"] != generation {
        bail!("pagination_cursor_expired: profile generation changed");
    }
    Ok(serde_json::from_value(value["source"].clone())?)
}
pub fn run(
    mut config: settings::File,
    selected: Option<&str>,
    server: Option<&str>,
    command: ProfileCommand,
) -> Result<()> {
    match command {
        ProfileCommand::Add {
            name,
            path,
            format,
            account_dir,
            keys_file,
        } => {
            settings::valid_name(&name)?;
            if format != "official-linux" {
                bail!(
                    "unsupported_profile_version: official-linux is the supported working format"
                );
            }
            if config.settings.local_profiles.contains_key(&name) {
                bail!("local profile already registered");
            }
            let profile = settings::LocalProfile {
                format,
                path: std::fs::canonicalize(path)?,
                account_dir,
                keys_file: keys_file.map(|p| {
                    if p.is_absolute() {
                        p
                    } else {
                        std::env::current_dir().unwrap().join(p)
                    }
                }),
            };
            let location = source(&profile)?;
            let missing = profile.keys_file.as_ref().is_none_or(|p| !p.is_file());
            config.settings.local_profiles.insert(name.clone(), profile);
            config.save()?;
            output(
                &json!({"localProfile":name,"state":"registered","path":location.root(),"keysState":if missing{"keys_missing"}else{"configured"}}),
            )
        }
        ProfileCommand::List => output(
            &json!({"items":config.settings.local_profiles,"standaloneDefault":config.settings.local_profile,"serverDefault":server.and_then(|s|config.settings.servers.get(s)).and_then(|s|s.local_profile.as_ref())}),
        ),
        ProfileCommand::Use { name } => {
            if !config.settings.local_profiles.contains_key(&name) {
                bail!("local profile not found");
            }
            if let Some(server) = server {
                config
                    .settings
                    .servers
                    .get_mut(server)
                    .context("saved server not found")?
                    .local_profile = Some(name.clone());
            } else {
                config.settings.local_profile = Some(name.clone());
            }
            config.save()?;
            output(&json!({"localProfile":name,"server":server}))
        }
        ProfileCommand::Keys {
            command:
                KeyCommand::Export {
                    name,
                    password_file,
                    from_agent_data,
                    agent_token_file,
                    session,
                    output: path,
                    source_stopped,
                },
        } => {
            let (name, profile) = config.profile(name.as_deref().or(selected), server)?;
            let location = source(&profile)?;
            destination(&profile, &location, &path)?;
            let opts = ReadOptions {
                stopped_or_consistent_snapshot: source_stopped,
                ..Default::default()
            };
            let keys = if let Some(password) = password_file {
                wechat_profile::keys_from_password(location, &private_input(&password, 32)?, opts)?
            } else if let Some(agent) = from_agent_data {
                let database = if agent.is_dir() {
                    agent.join("agent.db")
                } else {
                    agent
                };
                let token=agent_token_file.context("key_unavailable: --agent-token-file is required to unlock the copied agent cache")?;
                wechat_profile::keys_from_agent_cache(
                    location,
                    &database,
                    &private_input(&token, 16384)?,
                    session.as_deref(),
                    opts,
                )?
            } else {
                bail!("choose --password-file or --from-agent-data");
            };
            keys.write_new(&path)?;
            output(
                &json!({"state":"keys_exported","localProfile":name,"output":path,"validatedDatabases":keys.databases.len(),"imageKeyAvailable":keys.image_dat.is_some()}),
            )
        }
        command => {
            let (name, profile) = config.profile(selected, server)?;
            let location = source(&profile)?;
            if matches!(command, ProfileCommand::Inspect) {
                let mut files = vec![];
                inventory(
                    &location.root().join("db_storage"),
                    &location.root().join("db_storage"),
                    &mut files,
                    0,
                )?;
                return output(
                    &json!({"format":profile.format,"adapterVersion":1,"localProfile":name,"path":location.root(),"keysFile":profile.keys_file,"keysState":if profile.keys_file.as_ref().is_some_and(|p|p.is_file()){"configured"}else{"keys_missing"},"databases":files,"consistency":"content reads require --source-stopped or a consistent snapshot assertion"}),
                );
            }
            match command {
                ProfileCommand::Chats { read } => {
                    let view = reader(&profile, &read)?;
                    output(&wrap(
                        &name,
                        &view,
                        &view.threads(&ListQuery {
                            limit: Some(read.limit as usize),
                            cursor: read.cursor.clone(),
                            search: read.search.clone(),
                        })?,
                        read.raw,
                    )?)
                }
                ProfileCommand::Contacts { read } => {
                    let view = reader(&profile, &read)?;
                    output(&wrap(
                        &name,
                        &view,
                        &view.contacts(&ListQuery {
                            limit: Some(read.limit as usize),
                            cursor: read.cursor.clone(),
                            search: read.search.clone(),
                        })?,
                        read.raw,
                    )?)
                }
                ProfileCommand::Groups { id, members, read } => {
                    let view = reader(&profile, &read)?;
                    let query = ListQuery {
                        limit: Some(read.limit as usize),
                        cursor: read.cursor.clone(),
                        search: read.search.clone(),
                    };
                    if members {
                        output(&wrap(
                            &name,
                            &view,
                            &view.group_members(
                                id.as_deref().context("group ID required")?,
                                &query,
                            )?,
                            read.raw,
                        )?)
                    } else {
                        let mut page = view.groups(&query)?;
                        if let Some(id) = id {
                            let mut seen = std::collections::BTreeSet::new();
                            loop {
                                if let Some(found) =
                                    page.items.into_iter().find(|g| g.username == id)
                                {
                                    page.items = vec![found];
                                    page.next_cursor = None;
                                    break;
                                }
                                let Some(cursor) = page.next_cursor.take() else {
                                    page.items = vec![];
                                    break;
                                };
                                if !seen.insert(cursor.clone()) {
                                    bail!("group pagination cursor did not advance");
                                }
                                page = view.groups(&ListQuery {
                                    limit: Some(200),
                                    cursor: Some(cursor),
                                    search: None,
                                })?;
                            }
                            let mut value = wrap(&name, &view, &page, read.raw)?;
                            if page.items.is_empty() {
                                value["state"] = json!("not_found");
                            }
                            return output(&value);
                        }
                        output(&wrap(&name, &view, &page, read.raw)?)
                    }
                }
                ProfileCommand::Messages {
                    chat,
                    own_username,
                    read,
                } => {
                    let view = reader(&profile, &read)?;
                    let page = view.messages(&MessageQuery {
                        chat: Some(chat),
                        own_username,
                        limit: Some(read.limit as usize),
                        cursor: read.cursor.clone(),
                        message_type: None,
                    })?;
                    let mut value = wrap(&name, &view, &page, read.raw)?;
                    for (item, message) in value["items"]
                        .as_array_mut()
                        .context("invalid message projection")?
                        .iter_mut()
                        .zip(&page.items)
                    {
                        item["observationId"] =
                            json!(handle(&name, view.generation(), &message.id)?);
                    }
                    output(&value)
                }
                ProfileCommand::Images {
                    message,
                    quality,
                    output: path,
                    read,
                } => {
                    destination(&profile, &location, &path)?;
                    let view = reader(&profile, &read)?;
                    let id = parse_handle(&message, &name, view.generation())?;
                    let quality = match quality.as_str() {
                        "thumbnail" => ImageQuality::Thumbnail,
                        "standard" => ImageQuality::Standard,
                        "original" | "full" => ImageQuality::Original,
                        _ => ImageQuality::Best,
                    };
                    let image = view.cached_image(&id, quality)?;
                    private_output(&path, &image.media.bytes)?;
                    output(
                        &json!({"localProfile":name,"state":"available","output":path,"requestedQuality":quality,"quality":image.quality,"width":image.width,"height":image.height,"mediaType":image.media.media_type,"byteLength":image.media.bytes.len(),"sha256":image.media.sha256,"sourceView":view.generation()}),
                    )
                }
                ProfileCommand::Avatars {
                    username,
                    output: path,
                    read,
                } => {
                    destination(&profile, &location, &path)?;
                    let view = reader(&profile, &read)?;
                    let image = view.avatar(&username)?;
                    private_output(&path, &image.bytes)?;
                    output(
                        &json!({"localProfile":name,"state":"available","output":path,"mediaType":image.media_type,"byteLength":image.bytes.len(),"sha256":image.sha256,"sourceView":view.generation()}),
                    )
                }
                _ => unreachable!(),
            }
        }
    }
}
fn inventory(root: &Path, path: &Path, out: &mut Vec<Value>, depth: usize) -> Result<()> {
    if depth > 16 || out.len() > 4096 {
        bail!("profile inventory exceeds bound");
    }
    let mut entries = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            inventory(root, &entry.path(), out, depth + 1)?;
        } else if metadata.is_file() && entry.path().extension().is_some_and(|x| x == "db") {
            out.push(json!({"relativePath":entry.path().strip_prefix(root)?,"bytes":metadata.len(),"hasWal":PathBuf::from(format!("{}-wal",entry.path().display())).is_file()}));
        } else if !metadata.is_file() {
            bail!("unsupported profile link or special file");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn derivative_outputs_cannot_reenter_a_home_through_noncanonical_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let account = home.join("xwechat_files/test");
        std::fs::create_dir_all(account.join("db_storage")).unwrap();
        let profile = settings::LocalProfile {
            format: "official-linux".into(),
            path: home.join("../home"),
            account_dir: Some("test".into()),
            keys_file: None,
        };
        let location = source(&profile).unwrap();
        assert!(destination(&profile, &location, &home.join("exported-keys.json")).is_err());
        assert!(destination(&profile, &location, &temp.path().join("outside.json")).is_ok());
        assert!(!home.join("exported-keys.json").exists());
    }
    #[test]
    fn default_projection_redacts_raw_xml_and_source_fields() {
        let value = safe(
            json!({"messageType":3,"rawFields":{"key":"secret"},"source":{"text":"private signed URL"},"content":{"text":"<xml>private</xml>"}}),
            false,
        );
        assert!(value.get("rawFields").is_none());
        assert!(value.get("source").is_none());
        assert!(value["content"]["text"].is_null());
    }
    #[test]
    fn observation_handles_reject_cross_profile_and_changed_view() {
        let id = SourceMessageId {
            database: "message/message_0.db".into(),
            table: "Msg_fixture".into(),
            local_id: "1".into(),
        };
        let handle = handle("a", "v1", &id).unwrap();
        assert_eq!(parse_handle(&handle, "a", "v1").unwrap(), id);
        assert!(parse_handle(&handle, "b", "v1").is_err());
        assert!(parse_handle(&handle, "a", "v2").is_err());
    }
}
