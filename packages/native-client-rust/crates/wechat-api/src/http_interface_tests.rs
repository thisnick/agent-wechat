//! Generates `interfaces/http/routes.txt` and `interfaces/http/openapi.json`,
//! the reviewed listing of the daemon's HTTP routes. Both come from the route
//! table in `http_route_table.rs` and from `schemars` schemas of the typed
//! payloads, which follow the serde attributes of the real types.
//!
//! The router records every path it registers in test builds, so a route added
//! without a table entry fails `http_interface_route_table_is_complete`.
use crate::{
    http_route_table::{Body, Idempotency, Route, Scope, ROUTES},
    recorded_routes, router,
    schema_listing::{
        check_file, collapse_single_alternatives, to_json_text, Components, Direction, Row, Walker,
    },
    API_VERSION,
};
use schemars::generate::SchemaSettings;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use wechat_core::Core;
use wechat_store::Store;

const HEADER: &str = "# Daemon HTTP routes. Generated from the route table and payload types in crates/wechat-api; do not edit.\n# Run `pnpm interfaces` after changing a route or payload. Format: ITEM | ATTRIBUTES | DOC\n# `route` rows give the token scope, payloads and an SDK type where the TypeScript SDK declares one. A payload is `untyped` when the handler builds JSON by hand.\n# `request` and `response` rows list the fields of each typed payload. A type that the SDK declares but the handler does not return is listed too. Query strings are request fields.\n";

macro_rules! payloads {
    ($($name:literal => $ty:ty),+ $(,)?) => {
        /// The JSON schema of a payload type, generated for the direction it travels.
        fn payload_schema(name: &str, direction: Direction) -> Value {
            let generator = SchemaSettings::draft2020_12()
                .with(|settings| settings.contract = direction.contract())
                .into_generator();
            match name {
                $($name => generator.into_root_schema_for::<$ty>(),)+
                other => panic!("the route table names {other}, which has no schema; add it to payloads!"),
            }
            .to_value()
        }
    };
}

payloads! {
    "TokenRequest" => crate::TokenRequest,
    "CreateAccountRequest" => crate::CreateAccountRequest,
    "UpdateAccountRequest" => crate::actions::UpdateAccountRequest,
    "ContactLookupRequest" => crate::ContactLookupRequest,
    "EventAckRequest" => crate::EventAckRequest,
    "PageQuery" => crate::PageQuery,
    "QrQuery" => crate::QrQuery,
    "MediaQuery" => crate::MediaQuery,
    "RoomQuery" => crate::RoomQuery,
    "EventQuery" => crate::EventQuery,
    "SendKeyQuery" => crate::actions::SendKeyQuery,
    "GroupMutationKeyQuery" => crate::groups::GroupMutationKeyQuery,
    "ContentQuery" => crate::media::ContentQuery,
    "OfficialProfileRequest" => wechat_core::OfficialProfileRequest,
    "OfficialProfileSelection" => wechat_core::OfficialProfileSelection,
    "LoginRequest" => wechat_core::LoginRequest,
    "RefreshAvatarRequest" => crate::actions::RefreshAvatarRequest,
    "MediaTransferRequest" => wechat_core::MediaTransferRequest,
    "MediaSendRequest" => wechat_core::MediaSendRequest,
    "ShareSendRequest" => wechat_core::ShareSendRequest,
    "MediaJob" => wechat_core::media_types::MediaJob,
    "MediaAsset" => wechat_core::media_types::MediaAsset,
    "AccountSettings" => wechat_store::AccountSettings,
    "OwnedMediaSource" => wechat_store::OwnedMediaSource,
    "CallSnapshot" => wechat_core::call_types::CallSnapshot,
    "NativeStatus" => crate::contracts::NativeStatus,
    "NativeAccountRemovalRequest" => crate::contracts::NativeAccountRemovalRequest,
    "NativeAccountRemoval" => crate::contracts::NativeAccountRemoval,
    "NativeOperation" => crate::contracts::NativeOperation,
    "NativeReadRequest" => crate::contracts::NativeReadRequest,
    "NativeMediaBatchRequest" => crate::contracts::NativeMediaBatchRequest,
    "NativeMediaBatch" => crate::contracts::NativeMediaBatch,
    "NativeMiniPrograms" => crate::contracts::NativeMiniPrograms,
    "NativeMiniProgramLaunchRequest" => crate::contracts::NativeMiniProgramLaunchRequest,
    "NativeMiniProgramLaunch" => crate::contracts::NativeMiniProgramLaunch,
    "NativeMiniProgramPackage" => crate::contracts::NativeMiniProgramPackage,
    "NativeMiniProgramIpcRequest" => crate::contracts::NativeMiniProgramIpcRequest,
    "NativeMiniProgramLogin" => crate::contracts::NativeMiniProgramLogin,
    "NativeMediaJobRetryRequest" => crate::contracts::NativeMediaJobRetryRequest,
    "NativeMediaWritebackRequest" => crate::contracts::NativeMediaWritebackRequest,
    "NativeGroupCreateRequest" => crate::contracts::NativeGroupCreateRequest,
    "NativeGroupNameRequest" => crate::contracts::NativeGroupNameRequest,
    "NativeGroupMembersRequest" => crate::contracts::NativeGroupMembersRequest,
    "NativeGroupMyNameRequest" => crate::contracts::NativeGroupMyNameRequest,
    "NativeThreadNotificationsRequest" => crate::contracts::NativeThreadNotificationsRequest,
    "NativeThreadPinRequest" => crate::contracts::NativeThreadPinRequest,
    "NativeRecallRequest" => crate::contracts::NativeRecallRequest,
    "NativeForgetRequest" => crate::contracts::NativeForgetRequest,
    "NativeTextSendRequest" => crate::contracts::NativeTextSendRequest,
    "NativeSendIntent" => crate::contracts::NativeSendIntent,
    "NativeEvent" => crate::contracts::NativeEvent,
    "NativeMessage" => crate::contracts::NativeMessage,
    "NativeThread" => crate::contracts::NativeThread,
    "NativeContact" => crate::contracts::NativeContact,
    "NativeGroup" => crate::contracts::NativeGroup,
    "NativeGroupMemberLookup" => crate::contracts::NativeGroupMemberLookup,
    "NativeThreadPage" => crate::contracts::NativePage<crate::contracts::NativeThread>,
    "NativeContactPage" => crate::contracts::NativePage<crate::contracts::NativeContact>,
    "NativeGroupPage" => crate::contracts::NativePage<crate::contracts::NativeGroup>,
    "NativeGroupMemberPage" => crate::contracts::NativePage<crate::contracts::NativeGroupMember>,
    "NativeMessagePage" => crate::contracts::NativePage<crate::contracts::NativeMessage>,
}

/// The payload types an SDK declaration names. The SDK writes a page as
/// `NativePage<NativeThread>`; the schemas call it `NativeThreadPage`.
fn sdk_types(text: &str) -> Vec<String> {
    text.split('|')
        .map(|name| match name.strip_prefix("NativePage<") {
            Some(inner) => format!("{}Page", inner.trim_end_matches('>')),
            None => name.to_owned(),
        })
        .collect()
}

fn body_types(body: &Body) -> Vec<String> {
    match body {
        Body::Type(name) | Body::Optional(name) | Body::Sse(name) | Body::JsonOrSse(name) => {
            vec![(*name).to_owned()]
        }
        _ => Vec::new(),
    }
}

/// Types the daemon accepts, and types it returns, across every route.
fn payload_types() -> (BTreeSet<String>, BTreeSet<String>) {
    let (mut requests, mut responses) = (BTreeSet::new(), BTreeSet::new());
    for route in ROUTES {
        requests.extend(body_types(&route.request));
        requests.extend(body_types(&route.query));
        responses.extend(body_types(&route.response));
        for (role, types) in route.sdk {
            let target = if *role == "request" {
                &mut requests
            } else {
                &mut responses
            };
            target.extend(sdk_types(types));
        }
    }
    (requests, responses)
}

fn scope_attribute(scope: &Scope) -> String {
    match scope {
        Scope::None => "scope none".into(),
        Scope::Owner => "scope owner token".into(),
        Scope::One(scope) => format!("scope {scope}"),
        Scope::All(scopes) => format!("scope {}", scopes.join(" and ")),
        Scope::Depends { .. } => "scope varies".into(),
    }
}

fn payload_attribute(label: &str, body: &Body) -> Option<String> {
    Some(match body {
        Body::None => return None,
        Body::Untyped => format!("{label} untyped"),
        Body::Type(name) => format!("{label} {name}"),
        Body::Optional(name) => format!("{label} {name} (optional)"),
        Body::Multipart => format!("{label} multipart"),
        Body::Bytes("*/*") => format!("{label} bytes of the asset's media type"),
        Body::Bytes(mime) => format!("{label} {mime}"),
        Body::Sse(name) => format!("{label} sse {name}"),
        Body::JsonOrSse(name) => format!("{label} untyped or sse {name}"),
        Body::Upgrade(socket) => format!("upgrade websocket, socket {socket}"),
    })
}

fn route_row(route: &Route) -> Row {
    let mut attributes = vec![scope_attribute(&route.scope)];
    attributes.push(match route.idempotency {
        Idempotency::No => String::new(),
        Idempotency::Required => "idempotency key required".into(),
        Idempotency::Optional => "idempotency key optional".into(),
    });
    attributes.extend(payload_attribute("request", &route.request));
    attributes.extend(payload_attribute("query", &route.query));
    attributes.extend(payload_attribute("response", &route.response));
    if route.status != 200 {
        attributes.push(format!("status {}", route.status));
    }
    for role in ["request", "response"] {
        let types: Vec<&str> = route
            .sdk
            .iter()
            .filter(|(r, _)| *r == role)
            .map(|(_, t)| *t)
            .collect();
        if !types.is_empty() {
            attributes.push(format!("sdk {role} {}", types.join("|")));
        }
    }
    attributes.retain(|attribute| !attribute.is_empty());
    let mut doc = route.summary.to_owned();
    if let Scope::Depends { note, .. } = &route.scope {
        doc.push_str(&format!(" Scope: {note}"));
    }
    Row {
        item: format!("route {} {}", route.method, route.path),
        attributes,
        doc,
    }
}

fn payload_rows() -> Vec<Row> {
    let (requests, responses) = payload_types();
    let mut rows = Vec::new();
    for (direction, label, names) in [
        (Direction::Client, "request", requests),
        (Direction::Server, "response", responses),
    ] {
        for name in names {
            let mut root = payload_schema(&name, direction);
            let defs = match root
                .as_object_mut()
                .and_then(|object| object.remove("$defs"))
            {
                Some(Value::Object(defs)) => defs,
                _ => Map::new(),
            };
            let mut walker = Walker::new(&defs, direction);
            walker.walk(format!("{label} {name}"), &root, false, None);
            rows.extend(walker.rows);
        }
    }
    rows
}

fn listing() -> String {
    let mut rows: Vec<Row> = ROUTES.iter().map(route_row).collect();
    rows.extend(payload_rows());
    rows.sort_by(|a, b| a.item.cmp(&b.item));
    let mut text = HEADER.to_owned();
    for row in rows {
        text.push_str(&row.render());
        text.push('\n');
    }
    text
}

fn param_doc(path: &str, name: &str) -> String {
    let text = match name {
        "a" => "Account ID or alias.",
        "room" => "Group ID.",
        "thread" => "Chat ID.",
        "consumer" => "Name that identifies the event consumer.",
        "username" => "Contact ID of the group member.",
        "call" => "Call ID.",
        "app" => "Public Mini Program AppID.",
        "version" => "Exact cached package version from the launch response.",
        "package" => "Package ID from the launch response.",
        "id" => {
            let before = path.split("/{id}").next().unwrap_or("");
            match before.rsplit('/').next().unwrap_or("") {
                "tokens" | "access-tokens" => "Token ID.",
                "operations" => "Operation ID.",
                "login-attempts" => "Login attempt ID.",
                "threads" => "Chat ID.",
                "messages" => "Message ID.",
                "avatars" | "contacts" => "Contact ID.",
                "sends" => "Send ID.",
                "media" => "Media asset ID.",
                "jobs" => "Job ID.",
                "groups" => "Group ID.",
                "launches" => "Mini Program launch ID.",
                other => panic!("describe the {{id}} parameter after /{other}/"),
            }
        }
        other => panic!("describe the {{{other}}} path parameter"),
    };
    text.to_owned()
}

fn path_parameters(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| {
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "description": param_doc(path, name),
                "schema": {"type": "string"},
            })
        })
        .collect()
}

fn schema_link(components: &Components, direction: Direction, name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{}", components.name(direction, name))})
}

/// Untyped JSON has no schema; the marker tells readers it is not an omission.
fn untyped_content() -> Value {
    json!({"schema": {}, "x-agent-wechat-untyped": true})
}

fn request_body(components: &Components, route: &Route) -> Option<Value> {
    let (required, content) = match &route.request {
        Body::None => return None,
        Body::Untyped => (true, json!({"application/json": untyped_content()})),
        Body::Type(name) => (
            true,
            json!({"application/json": {"schema": schema_link(components, Direction::Client, name)}}),
        ),
        Body::Optional(name) => (
            false,
            json!({"application/json": {"schema": schema_link(components, Direction::Client, name)}}),
        ),
        Body::Multipart => (
            true,
            json!({"multipart/form-data": {"schema": {
                "type": "object",
                "required": ["file"],
                "properties": {"file": {
                    "type": "string",
                    "format": "binary",
                    "description": "The file to stage. Send exactly one part named file."
                }},
            }}}),
        ),
        other => panic!(
            "{} cannot be a request body: {}",
            route.path,
            payload_attribute("request", other).unwrap_or_default()
        ),
    };
    Some(json!({"required": required, "content": content}))
}

fn success_response(components: &Components, route: &Route) -> Value {
    let description = match route.status {
        101 => "The connection upgrades to a WebSocket.",
        201 => "Created.",
        202 => "Accepted. The work continues in the background.",
        _ => "Success.",
    };
    let mut response = json!({"description": description});
    let content = match &route.response {
        Body::None | Body::Optional(_) | Body::Multipart => return response,
        Body::Upgrade(_) => {
            return json!({"description": "The connection upgrades to a WebSocket."})
        }
        Body::Untyped => json!({"application/json": untyped_content()}),
        Body::Type(name) => {
            json!({"application/json": {"schema": schema_link(components, Direction::Server, name)}})
        }
        Body::Bytes(mime) => json!({*mime: {"schema": {"type": "string", "format": "binary"}}}),
        Body::Sse(name) => json!({"text/event-stream": sse_content(components, name)}),
        Body::JsonOrSse(name) => json!({
            "application/json": untyped_content(),
            "text/event-stream": sse_content(components, name),
        }),
    };
    response["content"] = content;
    response
}

fn sse_content(components: &Components, name: &str) -> Value {
    json!({
        "schema": {"type": "string", "description": format!("Server-sent events. Each event's data is a {name} JSON object.")},
        "x-agent-wechat-event-schema": schema_link(components, Direction::Server, name),
    })
}

fn operation(components: &Components, route: &Route) -> Value {
    let mut parameters = path_parameters(route.path);
    if let Body::Type(name) = &route.query {
        let mut chars = name.chars();
        let first = chars.next().map(|c| c.to_ascii_lowercase()).unwrap_or('q');
        parameters.push(json!({
            "name": format!("{first}{}", chars.as_str()),
            "in": "query",
            "style": "form",
            "explode": true,
            "description": "The fields of this object are separate query parameters.",
            "schema": schema_link(components, Direction::Client, name),
        }));
    }
    match &route.idempotency {
        Idempotency::No => {}
        kind => parameters.push(json!({
            "name": "Idempotency-Key",
            "in": "header",
            "required": matches!(kind, Idempotency::Required),
            "description": "Repeating a request with the same key returns the original result instead of repeating the work.",
            "schema": {"type": "string", "minLength": 1, "maxLength": 200},
        })),
    }
    let (security, note) = match &route.scope {
        Scope::None => (json!([]), None),
        Scope::Owner => (json!([{"bearer": ["owner"]}]), None),
        Scope::One(scope) => (json!([{"bearer": [scope]}]), None),
        Scope::All(scopes) => (json!([{"bearer": scopes}]), None),
        Scope::Depends { roles, note } => (json!([{"bearer": roles}]), Some(*note)),
    };
    let mut responses = Map::new();
    let status = if matches!(route.response, Body::Upgrade(_)) {
        101
    } else {
        route.status
    };
    responses.insert(status.to_string(), success_response(components, route));
    for range in ["4XX", "5XX"] {
        responses.insert(
            range.into(),
            json!({"$ref": "#/components/responses/Error"}),
        );
    }
    let mut operation = json!({
        "operationId": route.id,
        "summary": route.summary,
        "tags": [route.tag],
        "security": security,
        "parameters": parameters,
        "responses": responses,
    });
    if let Some(note) = note {
        operation["description"] = json!(format!("Scope: {note}"));
        operation["x-agent-wechat-scope"] = json!("depends on the request");
    }
    if let Some(body) = request_body(components, route) {
        operation["requestBody"] = body;
    }
    if let Body::Upgrade(socket) = &route.response {
        operation["x-agent-wechat-websocket"] = json!(format!("asyncapi.json#/channels/{socket}"));
    }
    if operation["parameters"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        operation.as_object_mut().unwrap().remove("parameters");
    }
    for (role, types) in route.sdk {
        let (key, direction) = if *role == "request" {
            ("x-agent-wechat-sdk-request", Direction::Client)
        } else {
            ("x-agent-wechat-sdk-response", Direction::Server)
        };
        let links: Vec<Value> = sdk_types(types)
            .iter()
            .map(|name| schema_link(components, direction, name))
            .collect();
        operation[key] = json!(links);
    }
    operation
}

const TAGS: &[(&str, &str)] = &[
    (
        "accounts",
        "Accounts, their settings and background operations.",
    ),
    ("auth", "Signing in, connecting and signing out."),
    (
        "calls",
        "Voice calls. The two sockets are described in asyncapi.json.",
    ),
    ("contacts", "Saved contacts and their avatars."),
    ("events", "The replayable event log and stream."),
    ("groups", "Saved groups, their members and group changes."),
    ("media", "Uploads, media assets, downloads and jobs."),
    ("messages", "Saved messages, sending and recall."),
    (
        "mini-programs",
        "Staged Mini Program packages, launches and app login codes.",
    ),
    ("system", "Daemon health, status and shutdown."),
    ("threads", "Chats, read state and chat settings."),
    (
        "tokens",
        "API tokens, which only the owner token can manage.",
    ),
];

fn openapi() -> Value {
    let (requests, responses) = payload_types();
    let mut components = Components::new();
    for (direction, names) in [
        (Direction::Client, &requests),
        (Direction::Server, &responses),
    ] {
        for name in names {
            components.add(direction, name, &payload_schema(name, direction));
        }
    }
    let mut schemas = components.schemas("#/components/schemas/");
    for schema in schemas.values_mut() {
        collapse_single_alternatives(schema);
    }
    schemas.insert(
        "ApiError".into(),
        json!({
            "description": "The body of every failed request.",
            "type": "object",
            "required": ["error"],
            "properties": {"error": {
                "type": "object",
                "required": ["code"],
                "properties": {"code": {"type": "string", "description": "A stable, machine-readable error code."}},
            }},
        }),
    );
    let mut paths: BTreeMap<&str, Map<String, Value>> = BTreeMap::new();
    for route in ROUTES {
        paths.entry(route.path).or_default().insert(
            route.method.to_ascii_lowercase(),
            operation(&components, route),
        );
    }
    json!({
        "openapi": "3.1.1",
        "info": {
            "title": "agent-wechat daemon API",
            "version": API_VERSION.to_string(),
            "description": "The HTTP API of the agent-wechat daemon. Every route except /health needs a bearer token. Call sockets are described in asyncapi.json.",
            "license": {"name": env!("CARGO_PKG_LICENSE"), "identifier": env!("CARGO_PKG_LICENSE")},
        },
        "servers": [{"url": "http://127.0.0.1:6174", "description": "The daemon listens on localhost by default."}],
        "tags": TAGS.iter().map(|(name, description)| json!({"name": name, "description": description})).collect::<Vec<_>>(),
        "paths": paths,
        "components": {
            "securitySchemes": {"bearer": {
                "type": "http",
                "scheme": "bearer",
                "description": "An API token sent as `Authorization: Bearer TOKEN`. The listed roles are token scopes, and `admin` passes every scope check. `owner` means only the owner token is accepted.",
            }},
            "responses": {"Error": {
                "description": "The request failed. The body carries a stable error code.",
                "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ApiError"}}},
            }},
            "schemas": schemas,
        },
    })
}

#[test]
fn http_interface_routes_listing_is_current() {
    check_file("routes.txt", &listing());
}

#[test]
fn http_interface_openapi_is_current() {
    check_file("openapi.json", &to_json_text(&openapi()));
}

#[test]
fn http_interface_listing_rows_have_unique_sorted_items() {
    let listing = listing();
    let items = listing
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split(" | ").next().unwrap())
        .collect::<Vec<_>>();
    assert!(
        items.windows(2).all(|pair| pair[0] < pair[1]),
        "rows must be sorted by item with no repeats"
    );
}

#[test]
fn http_interface_route_table_has_unique_routes_and_complete_docs() {
    let mut routes = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for route in ROUTES {
        assert!(
            routes.insert((route.method, route.path)),
            "{} {} is listed twice",
            route.method,
            route.path
        );
        assert!(
            ids.insert(route.id),
            "operation id {} is used twice",
            route.id
        );
        assert!(
            !route.summary.is_empty() && !route.summary.contains(" | "),
            "{} {}",
            route.method,
            route.path
        );
        assert!(
            TAGS.iter().any(|(tag, _)| *tag == route.tag),
            "unknown tag {}",
            route.tag
        );
    }
    // Every payload type resolves to a schema, and the documents hold no gaps.
    assert!(!openapi().to_string().is_empty());
}

fn paths_in_table() -> BTreeSet<&'static str> {
    ROUTES.iter().map(|route| route.path).collect()
}

const OWNER: &str = "synthetic-owner-api-token-for-http-listing-tests";

fn built_router() -> (axum::Router, Vec<&'static str>, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let core = Core::new(store, None).unwrap();
    let (app, registered) = recorded_routes::capture(|| router(core, OWNER));
    (app, registered, temp)
}

/// Adding a route with `api_route` but no table entry (or the reverse) fails here.
#[test]
fn http_interface_route_table_is_complete() {
    let (_app, registered, _temp) = built_router();
    let registered: BTreeSet<&str> = registered.into_iter().collect();
    let listed = paths_in_table();
    let missing: Vec<_> = registered.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "add these registered paths to http_route_table.rs: {missing:?}"
    );
    let stale: Vec<_> = listed.difference(&registered).collect();
    assert!(
        stale.is_empty(),
        "these table paths are not registered: {stale:?}"
    );
}

/// Every route must go through `api_route`; a bare `.route(` would skip the record.
#[test]
fn http_interface_routes_are_registered_through_the_recording_helper() {
    let sources = [
        ("lib.rs", include_str!("lib.rs")),
        ("media.rs", include_str!("media.rs")),
        ("groups.rs", include_str!("groups.rs")),
        ("actions.rs", include_str!("actions.rs")),
        ("calls.rs", include_str!("calls.rs")),
        ("miniprogram.rs", include_str!("miniprogram.rs")),
    ];
    for (file, source) in sources {
        for (number, line) in source.lines().enumerate() {
            if line.contains(".route(") && !line.contains("self.route(path, methods)") {
                panic!(
                    "{file}:{} registers a route without api_route: {line}",
                    number + 1
                );
            }
        }
    }
}

/// An unsupported method makes the router answer 405 with an `Allow` header
/// naming the methods that are registered, without running any handler.
#[tokio::test]
async fn http_interface_table_methods_match_the_router() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let (app, registered, _temp) = built_router();
    let mut by_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for route in ROUTES {
        by_path.entry(route.path).or_default().insert(route.method);
    }
    for path in registered.into_iter().collect::<BTreeSet<_>>() {
        let concrete = path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "sample"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("TRACE")
                    .uri(&concrete)
                    .header("authorization", format!("Bearer {OWNER}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            405,
            "{path} should answer an unsupported method with 405"
        );
        let allow = response
            .headers()
            .get("allow")
            .unwrap_or_else(|| panic!("{path} sent no Allow header"))
            .to_str()
            .unwrap()
            .to_owned();
        let served: BTreeSet<&str> = allow
            .split(',')
            .map(str::trim)
            .filter(|m| *m != "HEAD")
            .collect();
        assert_eq!(served, by_path[path], "methods of {path}");
    }
}

#[tokio::test]
async fn mini_program_ipc_admits_only_typed_requests_with_an_invocation_key() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    let (app, _, _temp) = built_router();
    let route = "/v1/accounts/one/mini-programs/wx0123456789abcdef/launches/00000000-0000-4000-8000-000000000001/ipc";
    for (body, key, expected) in [
        (r#"{"method":"wx.login"}"#, None, 400),
        (
            r#"{"method":"wx.pay"}"#,
            Some("00000000-0000-4000-8000-000000000002"),
            422,
        ),
        (
            r#"{"method":"wx.login","appId":"other"}"#,
            Some("00000000-0000-4000-8000-000000000002"),
            422,
        ),
        (
            r#"{"method":"wx.login"}"#,
            Some("00000000-0000-4000-8000-000000000002"),
            404,
        ),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri(route)
            .header("authorization", format!("Bearer {OWNER}"))
            .header("content-type", "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{body}");
    }
}
