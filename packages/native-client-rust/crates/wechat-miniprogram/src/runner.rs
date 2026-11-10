//! Shared assets for a browser or WebView host.

use crate::{valid_path, Archive, Framework};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;

// A service script has no browser print UI. Emscripten's shell detection
// otherwise treats window.print as a logger and replaces the app's console.
const SERVICE_GLOBALS: &str = "const print = undefined, printErr = undefined;\n";

/// License for the bundled Glass-easel JavaScript modules.
pub const GLASS_EASEL_LICENSE: &str = include_str!("../runner/vendor/LICENSE");

/// License for the bundled CSS syntax parser.
pub const CSS_TREE_LICENSE: &str = include_str!("../runner/vendor/CSS-TREE-LICENSE");

/// Find a built-in runner asset and its media type by request path.
pub fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    let javascript = "text/javascript; charset=utf-8";
    Some(match path {
        "/" => (
            "text/html; charset=utf-8",
            include_str!("../runner/index.html"),
        ),
        "/style.css" => (
            "text/css; charset=utf-8",
            include_str!("../runner/style.css"),
        ),
        "/runtime.js" => (javascript, include_str!("../runner/runtime.js")),
        "/bootstrap.js" => (javascript, include_str!("../runner/bootstrap.js")),
        "/intersection.js" => (javascript, include_str!("../runner/intersection.js")),
        "/styles.js" => (javascript, include_str!("../runner/styles.js")),
        "/images.js" => (javascript, include_str!("../runner/images.js")),
        "/css-tree.js" => (javascript, include_str!("../runner/vendor/css-tree.js")),
        "/legacy.js" => (javascript, include_str!("../runner/legacy.js")),
        "/adapter.js" => (javascript, include_str!("../runner/vendor/adapter.js")),
        "/glass-easel.js" => (javascript, include_str!("../runner/vendor/glass-easel.js")),
        _ => return None,
    })
}

/// Return a safe browser asset from the selected archive without extracting it.
pub fn package_asset<'a>(archive: &'a Archive, path: &str) -> Option<(&'static str, &'a [u8])> {
    if !valid_path(path) {
        return None;
    }
    let bytes = archive.file(path).ok()?;
    let (_, extension) = path.rsplit_once('.')?;
    let mime = match extension.to_ascii_lowercase().as_str() {
        "css" | "wxss" if bytes.len() <= 512 * 1024 && std::str::from_utf8(bytes).is_ok() => {
            "text/css; charset=utf-8"
        }
        "png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
        "jpg" | "jpeg" if bytes.starts_with(b"\xff\xd8\xff") => "image/jpeg",
        "webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => "image/webp",
        "gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => "image/gif",
        "svg" if bytes.len() <= 512 * 1024 && std::str::from_utf8(bytes).is_ok() => "image/svg+xml",
        "wasm" if bytes.len() <= 16 * 1024 * 1024 && bytes.starts_with(b"\0asm\x01\0\0\0") => {
            "application/wasm"
        }
        _ => return None,
    };
    Some((mime, bytes))
}

/// Whether this launch needs browser compilation of a packaged WebAssembly module.
pub fn has_wasm(archive: &Archive) -> bool {
    archive.entries.keys().any(|path| {
        path.ends_with(".wasm")
            && package_asset(archive, path).is_some_and(|(mime, _)| mime == "application/wasm")
    })
}

/// Build the runner's page module from one verified archive.
pub fn page_module(
    archive: &Archive,
    entry: &str,
    service: &str,
    query: &str,
    allow_login: bool,
) -> Result<String> {
    let route = serde_json::to_string(entry)?;
    let query = serde_json::to_string(query)?;
    let framework = archive.framework(entry, service)?;
    let mut module = format!(
        "{SERVICE_GLOBALS}export const entry = {route};\nexport const queryString = {query};\nexport const allowLogin = {allow_login};\nexport const framework = {:?};\n",
        framework.name()
    );
    let style_path = format!("{entry}.wxss");
    if package_asset(archive, &style_path).is_some() {
        module.push_str(&format!(
            "export const stylesheet = {};\n",
            serde_json::to_string(&format!("/{style_path}"))?,
        ));
    }
    match framework {
        Framework::GlassEasel => {
            let page = archive.compiled_page(entry, service)?;
            module.push_str(&format!(
                "export const templatesFactory = ({});\nexport const pageFactory = (Page, getApp) => ({});\n",
                page.templates, page.definition,
            ));
        }
        Framework::Exparser => {
            let page = archive.legacy_page(entry, service)?;
            let path = serde_json::to_string(&format!("./{entry}.wxml"))?;
            module.push_str(&format!(
                r#"
import {{ legacyTemplate }} from "/modules.js";
export const pageFactory = (Page, getApp) => ({definition});
export const templatesFactory = (route = {path}) => legacyTemplate(route, {generator});
"#,
                generator = serde_json::to_string(page.generator)?,
                definition = page.definition,
            ));
        }
    }
    Ok(module)
}

/// Assemble the independent CommonJS-style registrations found in a service module.
pub fn module_registry(archive: &Archive, service: &str) -> Result<String> {
    let (source, index) = archive.service(service)?;
    let config = if archive.entries.contains_key("app-config.json") {
        let config: serde_json::Value = serde_json::from_slice(archive.file("app-config.json")?)
            .context("Invalid Mini Program app configuration")?;
        ensure!(config.is_object(), "Invalid Mini Program app configuration");
        config
    } else {
        serde_json::json!({})
    };
    let style_path = service
        .strip_suffix("app-service.js")
        .context("Invalid service path")?
        .to_owned()
        + "app-wxss.js";
    let mut styles = if archive.entries.contains_key(&style_path) {
        crate::styles::Styles::parse(std::str::from_utf8(archive.file(&style_path)?)?)?.decode()?
    } else {
        Default::default()
    };
    for path in archive
        .entries
        .keys()
        .filter(|path| path.ends_with(".wxss"))
    {
        if let Some(("text/css; charset=utf-8", bytes)) = package_asset(archive, path) {
            styles
                .entry(path.trim_end_matches(".wxss").to_owned())
                .or_insert(std::str::from_utf8(bytes)?.to_owned());
        }
    }
    let component_configs: BTreeMap<_, _> =
        index.configs.iter().chain(&index.legacy_configs).collect();
    let mut output = format!(
        "{SERVICE_GLOBALS}export const compiledStyles = new Map(Object.entries(JSON.parse({})));\nexport const appConfig = JSON.parse({});\nexport const componentConfigs = new Map(Object.entries(JSON.parse({})));\nexport const registerModules = (App, getApp) => new Map([\n",
        serde_json::to_string(&serde_json::to_string(&styles)?)?,
        serde_json::to_string(&config.to_string())?,
        serde_json::to_string(&serde_json::to_string(&component_configs)?)?,
    );
    append_module_factories(&mut output, source, index, "")?;
    let (source, index) = archive.view_service(service)?;
    if let Some(prelude) = index
        .prelude
        .as_ref()
        .filter(|_| !index.generators.is_empty())
    {
        append_legacy_templates(&mut output, source, index, prelude.clone())?;
    }
    Ok(output)
}

/// Generate the factories for one verified subpackage without executing its scripts.
pub fn subpackage_module_registry(archive: &Archive, root: &str) -> Result<String> {
    ensure!(valid_path(root), "Invalid subpackage root");
    let prefix = format!("{root}/");
    let (source, index) = archive.service(&format!("{prefix}app-service.js"))?;
    let mut output =
        format!("{SERVICE_GLOBALS}export const registerModules = (App, getApp) => new Map([\n");
    append_module_factories(&mut output, source, index, &prefix)?;
    Ok(output)
}

fn append_module_factories(
    output: &mut String,
    source: &str,
    index: &crate::compiled::ServiceIndex,
    prefix: &str,
) -> Result<()> {
    for (path, module) in &index.modules {
        if !module.is_page && path.starts_with(prefix) {
            let path = serde_json::to_string(path)?;
            let factory = &source[module.factory.clone()];
            output.push_str(&format!("[{path}, ({factory})],\n"));
        }
    }
    output.push_str("]);\n");
    Ok(())
}

fn append_legacy_templates(
    output: &mut String,
    source: &str,
    index: &crate::compiled::ServiceIndex,
    prelude: std::ops::Range<usize>,
) -> Result<()> {
    let prelude = &source[prelude];
    let routes = serde_json::to_string(&serde_json::to_string(&index.legacy)?)?;
    output.push_str(&format!(
        r#"
import {{ compiler, compilerState as __WXML_GLOBAL__ }} from "/legacy.js";
const legacyRoutes = new Map(Object.entries(JSON.parse({routes})));
const legacyGenerators = new Map();
let legacyFactories;

export function legacyTemplate(route, fallback) {{
    if (!legacyFactories) {{
        const outerGlobal = globalThis;
        const __GWX_GLOBAL__ = {{}};
        const __wxAppCode__ = {{}};
        const __vd_version_info__ = {{ globalThis, delayedGwx: false }};
        (function() {{ {prelude} }}).call(compiler);
        const __g = compiler.__g;
        const $gaic = Object.create(null);
        let g = "", $gwxc = 0;
        legacyFactories = new Map([
"#
    ));
    for (name, span) in &index.generators {
        let name = serde_json::to_string(name)?;
        let expression = &source[span.clone()];
        output.push_str(&format!("[{name}, () => ({expression})],\n"));
    }
    output.push_str(
        r#"        ]);
    }
    const key = legacyRoutes.get(route) ?? fallback;
    if (!legacyGenerators.has(key)) legacyGenerators.set(key, legacyFactories.get(key)?.());
    return legacyGenerators.get(key)?.(route);
}
"#,
    );
    Ok(())
}
