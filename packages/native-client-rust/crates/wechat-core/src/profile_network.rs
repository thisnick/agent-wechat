//! Native saved business-host routing and guarded profile publication.
use super::saved_login::read_saved_file;
use super::*;
use std::path::{Path, PathBuf};

const HOST_ROOT: &str = ".xwechat/net/host";
const XML_LIMIT: u64 = 64 * 1024;

fn optional_file(home: &Path, relative: &Path, max: u64) -> Result<Option<Vec<u8>>> {
    safe_parent(home, relative, false)?;
    let path = home.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(Some(read_saved_file(&path, home, max)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// The official app does not honor our lease. The caller must already hold
// exclusive profile ownership; these checks also reject redirected ancestors.
fn safe_parent(home: &Path, relative: &Path, create: bool) -> Result<PathBuf> {
    use std::path::Component;
    if std::fs::canonicalize(home)? != home || relative.is_absolute() {
        bail!("invalid saved routing path");
    }
    let mut current = home.to_path_buf();
    for part in relative
        .parent()
        .context("invalid saved routing path")?
        .components()
    {
        let Component::Normal(part) = part else {
            bail!("invalid saved routing path")
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.is_dir() && !wechat_platform::is_link(&m) => {}
            Ok(_) => bail!("invalid saved routing path"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    continue;
                }
                match wechat_platform::private_dir(&current) {
                    Ok(()) => wechat_platform::sync_dir(current.parent().unwrap())?,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let m = std::fs::symlink_metadata(&current)?;
                        if !m.is_dir() || wechat_platform::is_link(&m) {
                            bail!("invalid saved routing path");
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(current)
}

fn selected_paths(home: &Path) -> Result<(PathBuf, PathBuf, Option<Vec<u8>>)> {
    let host_root = Path::new(HOST_ROOT);
    let ini = host_root.join("hostredirect.ini");
    let data = optional_file(home, &ini, 16 * 1024)?;
    let mut selected_key = String::from("default");
    if let Some(bytes) = &data {
        let mut section = "";
        for line in std::str::from_utf8(bytes)?.lines().map(str::trim) {
            if line.starts_with('[') && line.ends_with(']') {
                section = &line[1..line.len() - 1];
            } else if section == "default" {
                if let Some(("last_key", value)) =
                    line.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
                {
                    selected_key = value.to_owned();
                }
            }
        }
    }
    if selected_key.is_empty()
        || selected_key.len() > 128
        || !selected_key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        bail!("unsupported profile host selection");
    }
    let selected = host_root.join(&selected_key);
    let root = if selected_key != "default" && home.join(&selected).try_exists()? {
        selected
    } else {
        host_root.to_path_buf()
    };
    Ok((root.join("host-redirect.xml"), ini, data))
}

fn parse_hosts(data: &[u8]) -> Result<roxmltree::Document<'_>> {
    let document = roxmltree::Document::parse(std::str::from_utf8(data)?)?;
    if document.root_element().tag_name().name() != "hosts"
        || document.root_element().tag_name().namespace().is_some()
        || document.descendants().count() > 4096
    {
        bail!("unsupported profile host redirects");
    }
    Ok(document)
}

// These are persisted native host redirects, not active account credentials.
// Read only the main business pair; unrelated service mappings may legitimately
// contain names such as localhost and never become login destinations here.
pub(super) fn saved_business_hosts(home: &Path) -> Result<Option<(String, String)>> {
    let (xml, _, _) = selected_paths(home)?;
    let Some(data) = optional_file(home, &xml, XML_LIMIT)? else {
        return Ok(None);
    };
    let document = parse_hosts(&data)?;
    let mut selected = std::collections::BTreeMap::new();
    for mapping in document
        .root_element()
        .children()
        .filter(|n| n.is_element())
    {
        if mapping.tag_name().name() != "mapping" || mapping.tag_name().namespace().is_some() {
            continue;
        }
        let Some(source @ ("long.weixin.qq.com" | "short.weixin.qq.com")) =
            mapping.attribute("src")
        else {
            continue;
        };
        let destination = mapping
            .attribute("redirect")
            .context("missing host redirect")?;
        wechat_protocol::mmtls::validate_host(destination)?;
        if selected.insert(source, destination.to_owned()).is_some() {
            bail!("ambiguous profile host redirects");
        }
    }
    match (
        selected.remove("long.weixin.qq.com"),
        selected.remove("short.weixin.qq.com"),
    ) {
        (Some(long), Some(short)) => Ok(Some((long, short))),
        (None, None) => Ok(None),
        _ => bail!("incomplete profile host redirects"),
    }
}

#[derive(Serialize, Deserialize)]
struct RoutingFile {
    relative: PathBuf,
    before_sha256: Option<String>,
    after: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
pub(super) struct RoutingPlan {
    files: Vec<RoutingFile>,
}
fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// Retain unknown mappings, comments and launch settings byte for byte. Only
/// the two business destinations are projected from the accepted auth state.
pub(super) fn plan_routing(home: &Path, long: &str, short: &str) -> Result<RoutingPlan> {
    wechat_protocol::mmtls::validate_host(long)?;
    wechat_protocol::mmtls::validate_host(short)?;
    // Reject duplicate/partial known mappings before editing the document.
    saved_business_hosts(home)?;
    let (xml, ini, old_ini) = selected_paths(home)?;
    let old_xml = optional_file(home, &xml, XML_LIMIT)?;
    let mut after = old_xml.clone().unwrap_or_else(|| b"<hosts/>\n".to_vec());
    let doc = parse_hosts(&after)?;
    let mut replacements = Vec::new();
    for node in doc.root_element().children().filter(|n| {
        n.is_element() && n.tag_name().name() == "mapping" && n.tag_name().namespace().is_none()
    }) {
        let target = match node.attribute("src") {
            Some("long.weixin.qq.com") => long,
            Some("short.weixin.qq.com") => short,
            _ => continue,
        };
        let attr = node
            .attributes()
            .find(|a| a.name() == "redirect" && a.namespace().is_none())
            .context("missing host redirect")?;
        // Use the complete source range: range_value has a bounded whitespace
        // offset and is unsuitable for unusually spaced existing attributes.
        let range = attr.range();
        let raw = std::str::from_utf8(&after[range.clone()])?;
        let (name, rest) = raw.split_once('=').context("unsupported host attribute")?;
        let quoted = rest.trim_start();
        let quote = quoted
            .as_bytes()
            .first()
            .copied()
            .context("unsupported host attribute")?;
        if name.trim() != "redirect"
            || ![b'\'', b'"'].contains(&quote)
            || quoted.as_bytes().last() != Some(&quote)
        {
            bail!("unsupported host attribute");
        }
        let start = range.start + raw.len() - quoted.len() + 1;
        replacements.push((start..range.end - 1, target.to_owned()));
    }
    if replacements.is_empty() {
        let mapping = format!("\n  <mapping src=\"long.weixin.qq.com\" redirect=\"{long}\" type=\"main\"/>\n  <mapping src=\"short.weixin.qq.com\" redirect=\"{short}\" type=\"main\"/>\n");
        let range = doc.root_element().range();
        let root = std::str::from_utf8(&after[range.clone()])?;
        if root.ends_with("/>") {
            replacements.push((range.end - 2..range.end, format!(">{mapping}</hosts>")));
        } else {
            let close = range.start + root.rfind("</hosts").context("unsupported hosts root")?;
            replacements.push((close..close, mapping));
        }
    }
    drop(doc);
    for (range, text) in replacements.into_iter().rev() {
        after.splice(range, text.bytes());
    }
    if after.len() as u64 > XML_LIMIT {
        bail!("profile host redirects too large");
    }
    Ok(RoutingPlan {
        files: vec![
            RoutingFile {
                relative: xml,
                before_sha256: old_xml.as_deref().map(digest),
                after,
            },
            RoutingFile {
                relative: ini,
                before_sha256: old_ini.as_deref().map(digest),
                after: old_ini.unwrap_or_else(|| b"[default]\nlast_key=default\n".to_vec()),
            },
        ],
    })
}

/// XML precedes its selector, as in the native writer. Each file can be replayed
/// independently after a crash; the enclosing saved-login journal stays pending
/// until routing and credential readback both match the accepted generation.
pub(super) fn publish_routing(
    home: &Path,
    plan: &RoutingPlan,
    options: &wechat_profile::WriteOptions,
    mut guard: impl FnMut() -> Result<()>,
) -> Result<()> {
    use std::io::Write;
    use wechat_profile::ProfileError;
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired.into());
    }
    if plan.files.len() != 2 {
        bail!("invalid saved routing plan");
    }
    for entry in &plan.files {
        let name = entry.relative.file_name().and_then(|s| s.to_str());
        if !entry.relative.starts_with(HOST_ROOT)
            || !matches!(name, Some("host-redirect.xml" | "hostredirect.ini"))
            || entry.after.len() as u64 > XML_LIMIT
        {
            bail!("invalid saved routing plan");
        }
        guard()?;
        let current = optional_file(home, &entry.relative, XML_LIMIT)?;
        if current.as_deref() == Some(entry.after.as_slice()) {
            continue;
        }
        if current.as_deref().map(digest) != entry.before_sha256 {
            return Err(ProfileError::SourceChanged.into());
        }
        let parent = safe_parent(home, &entry.relative, true)?;
        let path = home.join(&entry.relative);
        let mut staged = tempfile::NamedTempFile::new_in(&parent)?;
        wechat_platform::protect(staged.as_file())?;
        staged.write_all(&entry.after)?;
        staged.as_file().sync_all()?;
        guard()?;
        if optional_file(home, &entry.relative, XML_LIMIT)?
            .as_deref()
            .map(digest)
            != entry.before_sha256
        {
            return Err(ProfileError::SourceChanged.into());
        }
        if current.is_some() {
            staged.persist(&path).map_err(|e| e.error)?;
        } else {
            staged.persist_noclobber(&path).map_err(|e| e.error)?;
        }
        wechat_platform::sync_dir(&parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> wechat_profile::WriteOptions {
        wechat_profile::WriteOptions {
            exclusive_ownership_confirmed: true,
            allow_plaintext: false,
            durable_intent_id: "synthetic-routing".into(),
        }
    }
    #[test]
    fn fresh_routing_replays_after_a_fence_between_xml_and_selector() {
        let temp = tempfile::tempdir().unwrap();
        let plan = plan_routing(temp.path(), "sglong.wechat.com", "sgshort.wechat.com").unwrap();
        let mut commits = 0;
        assert!(publish_routing(temp.path(), &plan, &options(), || {
            commits += 1;
            if commits == 3 {
                bail!("synthetic_logout_fence")
            } else {
                Ok(())
            }
        })
        .is_err());
        let root = temp.path().join(HOST_ROOT);
        assert!(root.join("host-redirect.xml").is_file());
        assert!(!root.join("hostredirect.ini").exists());
        publish_routing(temp.path(), &plan, &options(), || Ok(())).unwrap();
        assert_eq!(
            saved_business_hosts(temp.path()).unwrap(),
            Some(("sglong.wechat.com".into(), "sgshort.wechat.com".into()))
        );
        let before = std::fs::metadata(root.join("host-redirect.xml"))
            .unwrap()
            .modified()
            .unwrap();
        publish_routing(temp.path(), &plan, &options(), || Ok(())).unwrap();
        assert_eq!(
            std::fs::metadata(root.join("host-redirect.xml"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.join("hostredirect.ini"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn routing_preserves_other_services_and_selected_ini_and_refuses_changed_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(HOST_ROOT);
        std::fs::create_dir_all(root.join("selected")).unwrap();
        let ini = "[unrelated]\nvalue=opaque\n[default]\nlast_key=selected\n";
        std::fs::write(root.join("hostredirect.ini"), ini).unwrap();
        let xml = "<?xml version='1.0'?>\n<hosts unknown='keep'><!--retain--><mapping src='localhost' redirect='localhost'/><mapping src='short.weixin.qq.com' redirect = 'short.weixin.qq.com'/><mapping src='long.weixin.qq.com' redirect='long.weixin.qq.com'/></hosts>\n";
        let path = root.join("selected/host-redirect.xml");
        std::fs::write(&path, xml).unwrap();
        let plan = plan_routing(temp.path(), "sglong.wechat.com", "sgshort.wechat.com").unwrap();
        publish_routing(temp.path(), &plan, &options(), || Ok(())).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("hostredirect.ini")).unwrap(),
            ini
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            xml.replace(
                "redirect = 'short.weixin.qq.com'",
                "redirect = 'sgshort.wechat.com'"
            )
            .replace(
                "redirect='long.weixin.qq.com'",
                "redirect='sglong.wechat.com'"
            )
        );
        let plan = plan_routing(temp.path(), "long.wechat.com", "short.wechat.com").unwrap();
        let external = b"<hosts><!--external modification--></hosts>";
        std::fs::write(&path, external).unwrap();
        let error = publish_routing(temp.path(), &plan, &options(), || Ok(())).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<wechat_profile::ProfileError>(),
            Some(wechat_profile::ProfileError::SourceChanged)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), external);
    }
    #[test]
    fn routing_adds_missing_pair_and_rejects_redirected_ancestors_and_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(HOST_ROOT);
        std::fs::create_dir_all(&root).unwrap();
        let xml = "<hosts><!--unknown--><mapping src='localhost' redirect='localhost'/></hosts>";
        std::fs::write(root.join("host-redirect.xml"), xml).unwrap();
        let plan = plan_routing(temp.path(), "sglong.wechat.com", "sgshort.wechat.com").unwrap();
        let mut denied = options();
        denied.exclusive_ownership_confirmed = false;
        assert!(publish_routing(temp.path(), &plan, &denied, || Ok(())).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("host-redirect.xml")).unwrap(),
            xml
        );
        publish_routing(temp.path(), &plan, &options(), || Ok(())).unwrap();
        assert!(std::fs::read_to_string(root.join("host-redirect.xml"))
            .unwrap()
            .contains("<!--unknown--><mapping src='localhost' redirect='localhost'/>"));
        let alias = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(temp.path().join(".xwechat"), alias.path().join(".xwechat"))
            .unwrap();
        assert!(plan_routing(alias.path(), "sglong.wechat.com", "sgshort.wechat.com").is_err());
    }
    #[test]
    fn saved_routing_uses_selected_native_files_and_rejects_unsafe_or_ambiguous_pairs() {
        let temp = tempfile::tempdir().unwrap();
        assert!(saved_business_hosts(temp.path()).unwrap().is_none());
        let root = temp.path().join(".xwechat/net/host");
        std::fs::create_dir_all(&root).unwrap();
        let valid = r#"<hosts><mapping src="localhost" redirect="localhost" type="main"/><mapping src="long.weixin.qq.com" redirect="sglong.wechat.com" type="main"/><mapping src="short.weixin.qq.com" redirect="sgshort.wechat.com"/></hosts>"#;
        std::fs::write(root.join("host-redirect.xml"), valid).unwrap();
        let expected = Some(("sglong.wechat.com".into(), "sgshort.wechat.com".into()));
        assert_eq!(saved_business_hosts(temp.path()).unwrap(), expected);
        std::fs::write(
            root.join("hostredirect.ini"),
            "[default]\nlast_key=missing\n",
        )
        .unwrap();
        assert_eq!(saved_business_hosts(temp.path()).unwrap(), expected);
        std::fs::create_dir(root.join("selected")).unwrap();
        std::fs::write(
            root.join("selected/host-redirect.xml"),
            valid.replace("sglong", "long").replace("sgshort", "short"),
        )
        .unwrap();
        std::fs::write(
            root.join("hostredirect.ini"),
            "[default]\nlast_key=selected\n",
        )
        .unwrap();
        assert_eq!(
            saved_business_hosts(temp.path()).unwrap(),
            Some(("long.wechat.com".into(), "short.wechat.com".into()))
        );
        std::fs::write(
            root.join("hostredirect.ini"),
            "[default]\nlast_key=../outside\n",
        )
        .unwrap();
        assert!(saved_business_hosts(temp.path()).is_err());
        std::fs::write(
            root.join("hostredirect.ini"),
            "[default]\nlast_key=default\n",
        )
        .unwrap();
        for invalid in [
            valid.replace("sglong.wechat.com", "127.0.0.1"),
            valid.replace("sgshort.wechat.com", "untrusted.invalid"),
            valid.replace(
                "</hosts>",
                r#"<mapping src="long.weixin.qq.com" redirect="long.wechat.com"/></hosts>"#,
            ),
            r#"<hosts><mapping src="long.weixin.qq.com" redirect="sglong.wechat.com"/></hosts>"#
                .into(),
            format!("<!DOCTYPE hosts [<!ENTITY x 'sglong.wechat.com'>]>{valid}"),
            "x".repeat(64 * 1024 + 1),
        ] {
            std::fs::write(root.join("host-redirect.xml"), invalid).unwrap();
            assert!(saved_business_hosts(temp.path()).is_err());
        }
    }
}
