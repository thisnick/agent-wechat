//! Account-scoped cached Mini Programs and launch grants.

use crate::Core;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;
use wechat_miniprogram::{valid_app_id, verify_digest, Archive, CachedPackage, MAX_PACKAGE_BYTES};
use zeroize::Zeroizing;

/// Public display and routing metadata derived from a saved Mini Program card.
#[derive(Clone, Deserialize, Serialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MiniProgramCard {
    /// Public Mini Program AppID.
    pub app_id: String,
    /// Program name, when supplied by the card.
    pub display_name: Option<String>,
    /// Shared page title.
    pub title: Option<String>,
    /// Page route and query carried in the card.
    pub page_path: Option<String>,
}

pub(crate) struct ParsedMiniProgram {
    pub card: MiniProgramCard,
    pub preview: Option<crate::links::LinkThumbnail>,
    pub icon: Option<crate::links::LinkThumbnail>,
}

pub(crate) fn parse_card(app: roxmltree::Node<'_, '_>, subtype: i32) -> Option<ParsedMiniProgram> {
    if !matches!(subtype, 33 | 36) {
        return None;
    }
    let program = crate::quotes::child(app, "weappinfo")?;
    let app_id = crate::links::text(program, "appid", 18).filter(|id| valid_app_id(id))?;
    Some(ParsedMiniProgram {
        card: MiniProgramCard {
            app_id,
            display_name: crate::links::text(app, "sourcedisplayname", 256),
            title: crate::links::text(app, "title", 1024),
            page_path: crate::links::text(program, "pagepath", 4096)
                .filter(|value| wechat_miniprogram::split_page_path(value).is_some()),
        },
        preview: crate::links::thumbnail(app),
        icon: crate::links::text(program, "weappiconurl", 8192)
            .filter(|url| wechat_protocol::media::validate_link_thumbnail_url(url).is_ok())
            .map(crate::links::LinkThumbnail::Http),
    })
}

/// A Mini Program known from this account's staged package cache.
pub struct MiniProgramSummary {
    /// Public Mini Program AppID.
    pub app_id: String,
    /// Display name in the known-app list.
    pub title: String,
    /// Whether a package is present with a valid staging manifest.
    pub available: bool,
}

/// Verified package selection and temporary login grant for one launch.
pub struct MiniProgramLaunch {
    /// ID that binds this app and package to its initiating client.
    pub id: Uuid,
    /// Metadata of the exact staged archive.
    pub package: CachedPackage,
    /// Size of the verified archive.
    pub size_bytes: usize,
    /// Available verified-version subpackage descriptors.
    pub subpackages: Vec<MiniProgramSubpackage>,
    /// Query carried in the saved card, without its leading question mark.
    pub query: String,
    /// Compiled component framework selected by the archive.
    pub framework: wechat_miniprogram::Framework,
    /// Whether the caller can request login codes on this launch.
    pub login: bool,
}

/// A locally staged archive for one root declared by the main package.
pub struct MiniProgramSubpackage {
    /// The package cache identifier.
    pub id: String,
    /// Root path declared in the main package configuration.
    pub root: String,
    /// SHA-256 of the staged archive.
    pub sha256: String,
    /// Byte size reported by the staged archive file.
    pub size_bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubpackageManifest {
    root: String,
    version: String,
    sha256: String,
}

#[derive(Default)]
pub(crate) struct Launches(Mutex<BTreeMap<Uuid, Grant>>);

struct Grant {
    account: String,
    app: String,
    principal: String,
    digest: String,
    expires: Instant,
    logout_epoch: Option<Vec<u8>>,
    invocations: BTreeMap<Uuid, Arc<LoginInvocation>>,
}

type LoginInvocation = OnceLock<Option<Zeroizing<String>>>;

fn directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(meta.is_dir(), "mini_program_cache_invalid");
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "mini_program_cache_invalid"
    );
    let file = wechat_platform::OpenOptions::new().read(true).open(path)?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "mini_program_cache_invalid"
    );
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "mini_program_cache_invalid");
    Ok(bytes)
}

fn manifest(dir: &Path) -> Result<CachedPackage> {
    let metadata: CachedPackage = serde_json::from_slice(&file(&dir.join("manifest.json"), 8192)?)
        .context("mini_program_cache_invalid")?;
    metadata.validate().context("mini_program_cache_invalid")?;
    Ok(metadata)
}

fn valid_package_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn subpackage_metadata(dir: &Path, id: &str, version: &str) -> Result<MiniProgramSubpackage> {
    ensure!(valid_package_id(id), "mini_program_package_unavailable");
    let parent = dir.join("subpackages");
    let path = parent.join(id);
    ensure!(
        directory(&parent)? && directory(&path)?,
        "mini_program_package_unavailable"
    );
    let metadata: SubpackageManifest =
        serde_json::from_slice(&file(&path.join("manifest.json"), 8192)?)
            .context("mini_program_cache_invalid")?;
    ensure!(
        metadata.version == version,
        "mini_program_package_unavailable"
    );
    let root = metadata.root;
    ensure!(
        wechat_miniprogram::split_page_path(&root) == Some((root.as_str(), ""))
            && metadata.sha256.len() == 64
            && metadata
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "mini_program_cache_invalid"
    );
    let archive = fs::symlink_metadata(path.join("package.wxapkg"))
        .context("mini_program_package_unavailable")?;
    ensure!(
        archive.is_file() && (18..=MAX_PACKAGE_BYTES as u64).contains(&archive.len()),
        "mini_program_cache_invalid"
    );
    Ok(MiniProgramSubpackage {
        id: id.to_owned(),
        root,
        sha256: metadata.sha256,
        size_bytes: archive.len() as usize,
    })
}

fn staged_subpackages(
    dir: &Path,
    archive: &Archive,
    package: &CachedPackage,
) -> Result<Vec<MiniProgramSubpackage>> {
    let parent = dir.join("subpackages");
    if !directory(&parent)? {
        return Ok(Vec::new());
    }
    let config: serde_json::Value = serde_json::from_slice(archive.file("app-config.json")?)
        .context("mini_program_cache_invalid")?;
    let declared = config
        .get("subPackages")
        .or_else(|| config.get("subpackages"))
        .and_then(serde_json::Value::as_array);
    let mut packages = Vec::<MiniProgramSubpackage>::new();
    for (count, entry) in fs::read_dir(parent)?.enumerate() {
        ensure!(count < 64, "mini_program_cache_limit");
        let id = entry?.file_name().to_string_lossy().into_owned();
        ensure!(id != package.package_id, "mini_program_cache_invalid");
        let subpackage = subpackage_metadata(dir, &id, &package.version)?;
        ensure!(
            declared.is_some_and(|items| items.iter().any(|item| item["root"]
                .as_str()
                .is_some_and(|root| root.trim_end_matches('/') == subpackage.root)))
                && !packages.iter().any(|item| item.root == subpackage.root),
            "mini_program_cache_invalid"
        );
        packages.push(subpackage);
    }
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(packages)
}

fn write_cache_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

impl Core {
    /// Fetch a missing release archive from WeChat and publish its cache entry.
    /// An existing staged package is verified and reused.
    pub fn fetch_mini_program(
        &self,
        account: &str,
        app: &str,
        authorize: impl Fn() -> bool,
    ) -> Result<(CachedPackage, usize)> {
        ensure!(valid_app_id(app), "invalid_mini_program_app");
        let account = &self.store.account(account)?.id;
        let parent = self.store.root.join("miniprograms");
        let cache = parent.join(account);
        let destination = cache.join(app);
        let cached = || {
            let (dir, metadata) = self.cached_mini_program(account, app)?;
            let bytes = file(&dir.join("package.wxapkg"), MAX_PACKAGE_BYTES)
                .context("mini_program_cache_invalid")?;
            let size = bytes.len();
            Archive::open(app, bytes, &metadata.sha256).context("mini_program_cache_invalid")?;
            Ok((metadata, size))
        };
        ensure!(authorize(), "mini_program_download_forbidden");
        if directory(&parent)? && directory(&cache)? && directory(&destination)? {
            return cached();
        }
        let epoch = self.store.runtime(account, "logout-epoch")?;
        let check = || {
            ensure!(authorize(), "mini_program_download_forbidden");
            ensure!(
                self.mini_program_login_available(account)?
                    && self.store.runtime(account, "logout-epoch")? == epoch,
                "mini_program_download_offline"
            );
            Ok(())
        };
        check()?;
        let (version, bytes) = self
            .native_account(account, Default::default())?
            .mini_program_download_guarded(app, check)
            .context("mini_program_download_failed")?;
        check()?;
        let size = bytes.len();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if !directory(&parent)? {
            wechat_platform::private_dir(&parent)?;
        }
        if !directory(&cache)? {
            wechat_platform::private_dir(&cache)?;
        }
        let staged = wechat_platform::private_tempdir_in(&cache, ".fetch-")?;
        write_cache_file(&staged.path().join("package.wxapkg"), &bytes)?;
        let archive = Archive::open(app, bytes, &digest).context("mini_program_cache_invalid")?;
        let config: serde_json::Value = serde_json::from_slice(archive.file("app-config.json")?)
            .context("mini_program_page_unsupported")?;
        let entry = config["entryPagePath"]
            .as_str()
            .and_then(wechat_miniprogram::split_page_path)
            .context("mini_program_page_unsupported")?
            .0;
        let title = config
            .pointer("/global/window/navigationBarTitleText")
            .and_then(serde_json::Value::as_str)
            .filter(|title| {
                !title.trim().is_empty()
                    && title.len() <= 256
                    && !title.chars().any(char::is_control)
            })
            .unwrap_or(app);
        let metadata = CachedPackage {
            title: title.to_owned(),
            version: version.to_string(),
            package_id: "main".into(),
            sha256: digest,
            entry_page: entry.to_owned(),
            service_path: "app-service.js".into(),
            allow_login: false,
        };
        archive
            .file(&metadata.service_path)
            .context("mini_program_page_unsupported")?;
        metadata.validate().context("mini_program_cache_invalid")?;
        write_cache_file(
            &staged.path().join("manifest.json"),
            &serde_json::to_vec(&metadata)?,
        )?;
        wechat_platform::sync_dir(staged.path())?;
        check()?;
        match wechat_platform::publish_directory(staged.path(), &destination) {
            Ok(()) => Ok((metadata, size)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => cached(),
            Err(error) => Err(error.into()),
        }
    }

    fn mini_program_cache(&self, account: &str) -> Result<Option<PathBuf>> {
        let account = self.store.account(account)?;
        let cache = self.store.root.join("miniprograms");
        let account = cache.join(account.id);
        Ok((directory(&cache)? && directory(&account)?).then_some(account))
    }

    fn cached_mini_program(&self, account: &str, app: &str) -> Result<(PathBuf, CachedPackage)> {
        ensure!(valid_app_id(app), "invalid_mini_program_app");
        let cache = self
            .mini_program_cache(account)?
            .context("mini_program_package_unavailable")?;
        let dir = cache.join(app);
        ensure!(directory(&dir)?, "mini_program_package_unavailable");
        let metadata = manifest(&dir)?;
        Ok((dir, metadata))
    }

    /// List staged packages and programs seen in recent saved cards.
    pub fn mini_programs(&self, account: &str) -> Result<Vec<MiniProgramSummary>> {
        let account = &self.store.account(account)?.id;
        let mut items = BTreeMap::new();
        if let Some(cache) = self.mini_program_cache(account)? {
            for (index, entry) in fs::read_dir(cache)?.enumerate() {
                ensure!(index < 256, "mini_program_cache_limit");
                let entry = entry?;
                let app_id = entry.file_name().to_string_lossy().into_owned();
                if valid_app_id(&app_id) && entry.file_type()?.is_dir() {
                    let metadata = manifest(&entry.path()).ok();
                    let available = metadata.is_some()
                        && fs::symlink_metadata(entry.path().join("package.wxapkg")).is_ok_and(
                            |file| {
                                file.is_file()
                                    && (18..=MAX_PACKAGE_BYTES as u64).contains(&file.len())
                            },
                        );
                    items.insert(
                        app_id.clone(),
                        MiniProgramSummary {
                            title: metadata.map_or_else(|| app_id.clone(), |item| item.title),
                            app_id,
                            available,
                        },
                    );
                }
            }
        }
        if let Ok(reader) = self.profile_reader_owned(account) {
            let page = reader.recent_mini_program_messages(100)?;
            for message in page.items {
                let Some(card) =
                    crate::quotes::message_app(&message).and_then(|app| app.mini_program)
                else {
                    continue;
                };
                if items.contains_key(&card.card.app_id) {
                    continue;
                }
                let id = crate::message_id(&message.id)?;
                if self
                    .ensure_message_not_recalled_with_reader(account, &id, &reader)
                    .is_err()
                {
                    continue;
                }
                items.insert(
                    card.card.app_id.clone(),
                    MiniProgramSummary {
                        title: card
                            .card
                            .display_name
                            .or(card.card.title)
                            .unwrap_or_else(|| card.card.app_id.clone()),
                        app_id: card.card.app_id,
                        available: false,
                    },
                );
            }
        }
        Ok(items.into_values().collect())
    }

    /// Verify the selected package and prepare its optional login grant.
    pub fn prepare_mini_program(
        &self,
        account: &str,
        app: &str,
        principal: &str,
        can_login: bool,
        source_message: Option<&str>,
    ) -> Result<MiniProgramLaunch> {
        let account = &self.store.account(account)?.id;
        let (dir, mut package) = self.cached_mini_program(account, app)?;
        let mut query = String::new();
        if let Some(source) = source_message {
            let message = self
                .message(account, source)
                .context("mini_program_source_unavailable")?;
            let card: MiniProgramCard = serde_json::from_value(message["miniProgram"].clone())
                .context("mini_program_source_unavailable")?;
            ensure!(card.app_id == app, "mini_program_source_unavailable");
            let (path, page_query) = card
                .page_path
                .as_deref()
                .and_then(wechat_miniprogram::split_page_path)
                .context("mini_program_page_unsupported")?;
            package.entry_page = path.to_owned();
            query = page_query.to_owned();
        }
        let bytes = file(&dir.join("package.wxapkg"), MAX_PACKAGE_BYTES)
            .context("mini_program_package_unavailable")?;
        let size_bytes = bytes.len();
        let archive =
            Archive::open(app, bytes, &package.sha256).context("mini_program_cache_invalid")?;
        let framework = archive
            .framework(&package.entry_page, &package.service_path)
            .context("mini_program_page_unsupported")?;
        let subpackages = staged_subpackages(&dir, &archive, &package)?;
        let id = Uuid::new_v4();
        let login =
            can_login && package.allow_login && self.mini_program_login_available(account)?;
        if login {
            let now = Instant::now();
            let mut grants = self
                .mini_program_launches
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("mini_program_launch_unavailable"))?;
            grants.retain(|_, grant| grant.expires > now);
            ensure!(grants.len() < 128, "mini_program_launch_limit");
            grants.insert(
                id,
                Grant {
                    account: account.into(),
                    app: app.into(),
                    principal: principal.into(),
                    digest: package.sha256.clone(),
                    expires: now + Duration::from_secs(600),
                    logout_epoch: self.store.runtime(account, "logout-epoch")?,
                    invocations: BTreeMap::new(),
                },
            );
        }
        Ok(MiniProgramLaunch {
            id,
            package,
            size_bytes,
            subpackages,
            query,
            framework,
            login,
        })
    }

    /// Read the exact package version and validate its digest before returning bytes.
    pub fn mini_program_package(
        &self,
        account: &str,
        app: &str,
        version: &str,
        id: &str,
    ) -> Result<Vec<u8>> {
        let (dir, metadata) = self.cached_mini_program(account, app)?;
        ensure!(
            metadata.version == version,
            "mini_program_package_unavailable"
        );
        let (path, digest) = if metadata.package_id == id {
            (dir.join("package.wxapkg"), metadata.sha256)
        } else {
            let subpackage = subpackage_metadata(&dir, id, version)?;
            (
                dir.join("subpackages").join(id).join("package.wxapkg"),
                subpackage.sha256,
            )
        };
        let bytes = file(&path, MAX_PACKAGE_BYTES).context("mini_program_package_unavailable")?;
        verify_digest(&bytes, &digest).context("mini_program_cache_invalid")?;
        Ok(bytes)
    }

    fn mini_program_login_available(&self, account: &str) -> Result<bool> {
        Ok(self.upstream.uses_account_owner()
            && self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                == Some(self.ownership_generation.as_bytes())
            && self.store.runtime(account, "desired")?.as_deref() == Some(b"connected"))
    }

    /// Read this app's current server network policy through the account owner.
    ///
    /// This does not obtain a login code or retain a second copy of app attributes.
    /// Offline accounts return no policy; failed online lookups remain errors.
    pub fn mini_program_network_policy(
        &self,
        account: &str,
        app: &str,
        authorize: impl Fn() -> bool,
    ) -> Result<Option<wechat_protocol::MiniProgramNetworkPolicy>> {
        self.cached_mini_program(account, app)?;
        if !self.mini_program_login_available(account)? {
            return Ok(None);
        }
        let logout_epoch = self.store.runtime(account, "logout-epoch")?;
        let check = || {
            ensure!(
                authorize()
                    && self.mini_program_login_available(account)?
                    && self.store.runtime(account, "logout-epoch")? == logout_epoch,
                "mini_program_network_unavailable"
            );
            Ok(())
        };
        check()?;
        let owner = self.native_account(account, Default::default())?;
        let policy = owner
            .mini_program_network_policy_guarded(app, check)
            .context("mini_program_network_unavailable")?;
        check()?;
        Ok(Some(policy))
    }

    /// Issue or replay a login code for one invocation of an authorized launch.
    pub fn mini_program_login(
        &self,
        account: &str,
        app: &str,
        principal: &str,
        id: Uuid,
        invocation_id: Uuid,
        authorize: impl Fn() -> bool,
    ) -> Result<Zeroizing<String>> {
        let account = &self.store.account(account)?.id;
        let (_, package) = self.cached_mini_program(account, app)?;
        let logout_epoch = self.store.runtime(account, "logout-epoch")?;
        let check = || {
            ensure!(
                authorize()
                    && self.mini_program_login_available(account)?
                    && self.store.runtime(account, "logout-epoch")? == logout_epoch,
                "mini_program_login_forbidden"
            );
            Ok(())
        };
        ensure!(package.allow_login, "mini_program_login_forbidden");
        check()?;
        let (invocation, expires) = {
            let mut grants = self
                .mini_program_launches
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("mini_program_launch_unavailable"))?;
            let grant = grants
                .get_mut(&id)
                .filter(|grant| {
                    grant.expires > Instant::now()
                        && grant.account == *account
                        && grant.app == app
                        && grant.principal == principal
                        && grant.digest == package.sha256
                        && grant.logout_epoch == logout_epoch
                })
                .context("mini_program_launch_unavailable")?;
            let invocation = if let Some(invocation) = grant.invocations.get(&invocation_id) {
                Arc::clone(invocation)
            } else {
                ensure!(grant.invocations.len() < 32, "mini_program_launch_limit");
                Arc::clone(grant.invocations.entry(invocation_id).or_default())
            };
            (invocation, grant.expires)
        };
        let check = || {
            ensure!(Instant::now() < expires, "mini_program_launch_unavailable");
            check()
        };
        // A failed submission is also a terminal outcome for this invocation.
        // Grants are process-local, so a restart rejects an old launch ID.
        let code = invocation.get_or_init(|| {
            let owner = self.native_account(account, Default::default()).ok()?;
            owner.mini_program_login_guarded(app, check).ok()
        });
        check()?;
        code.clone().context("mini_program_upstream_login_failed")
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn reads_card_routes_without_exposing_transport_metadata() {
        let xml = r#"<msg><appmsg><type>36</type><title>Cafe menu</title>
            <sourcedisplayname>Cafe</sourcedisplayname><weappinfo>
            <appid>wx0123456789abcdef</appid>
            <pagepath>pages/menu/index.html?menu=seasonal&amp;table=2</pagepath>
            <weappiconurl>https://example.invalid/icon.png</weappiconurl>
            <username>private-transport-identity</username>
            </weappinfo></appmsg></msg>"#;
        let read = |xml: &str| crate::quotes::app_message(xml).and_then(|app| app.mini_program);
        let card = read(xml).unwrap().card;
        assert_eq!(card.title.as_deref(), Some("Cafe menu"));
        assert_eq!(
            card.page_path
                .as_deref()
                .and_then(wechat_miniprogram::split_page_path),
            Some(("pages/menu/index", "menu=seasonal&table=2"))
        );
        let json = serde_json::to_string(&card).unwrap();
        assert!(!json.contains("private-transport-identity"));
        assert!(!json.contains("icon.png"));

        let traversal = xml.replace("pages/menu/index.html", "../menu");
        assert!(read(&traversal).unwrap().card.page_path.is_none());
        assert!(read(&xml.replace("<type>36", "<type>5")).is_none());
        assert!(read(&xml.replace("wx0123456789abcdef", "../invalid")).is_none());
    }
}
