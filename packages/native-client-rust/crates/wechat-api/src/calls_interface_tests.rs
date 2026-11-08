//! Generates `interfaces/http/calls-websocket.txt`, the reviewed listing of the
//! calls WebSocket protocol: the sockets, the messages each side sends and every
//! field of those messages. See `interfaces/README.md` for the line format.
//!
//! The messages are the serde enums in `wechat_core::call_types`. Their JSON
//! schemas come from `schemars`, which reads the serde attributes, so the
//! listing follows the real wire format. `calls::SOCKETS` says which socket
//! carries which message.
use crate::{
    calls::{CallSocket, SocketMessage, Variants, SOCKETS},
    router,
    schema_listing::{
        check_file, collapse_single_alternatives, describe, literal, required_keys, to_json_text,
        Components, Direction, Row, Walker,
    },
};
use schemars::generate::SchemaSettings;
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};
use wechat_core::{
    call_types::{CallClientHello, CallCommand, CallEvent, CallMediaCommand},
    Core,
};
use wechat_store::Store;

const UPDATE: &str = "AGENT_WECHAT_UPDATE_INTERFACES";
const TAG: &str = "type";
const HEADER: &str = "# Calls WebSocket protocol. Generated from the message types and socket table in crates/wechat-api; do not edit.\n# Run `pnpm interfaces` after changing the call messages or sockets. Format: ITEM | ATTRIBUTES | DOC\n# `client` messages are sent to the daemon, `server` messages come from it. Message items are TYPE[tag], the value of `type`; nested fields use dots and `[]` marks array items.\n# `when kind x` limits a field to one variant of a nested message. `nullable` means JSON null is allowed. Server rows describe what the daemon sends, client rows what it accepts.\n";

fn root_schema(type_name: &str, direction: Direction) -> Value {
    let generator = SchemaSettings::draft2020_12()
        .with(|settings| settings.contract = direction.contract())
        .into_generator();
    match type_name {
        "CallClientHello" => generator.into_root_schema_for::<CallClientHello>(),
        "CallCommand" => generator.into_root_schema_for::<CallCommand>(),
        "CallEvent" => generator.into_root_schema_for::<CallEvent>(),
        "CallMediaCommand" => generator.into_root_schema_for::<CallMediaCommand>(),
        other => {
            panic!("the socket table names {other}, which has no schema; add it to root_schema")
        }
    }
    .to_value()
}
/// The variants a socket side sends, checked against the schema so the table
/// cannot name a tag that does not exist.
fn selected_tags(message: &SocketMessage, available: &[String]) -> Vec<String> {
    let known = |tags: &[&str]| {
        for tag in tags {
            assert!(
                available.iter().any(|name| name == tag),
                "{} has no variant {tag}",
                message.type_name
            );
        }
    };
    match &message.variants {
        Variants::All => available.to_vec(),
        Variants::Only(tags) => {
            known(tags);
            available
                .iter()
                .filter(|name| tags.contains(&name.as_str()))
                .cloned()
                .collect()
        }
        Variants::Except(tags) => {
            known(tags);
            available
                .iter()
                .filter(|name| !tags.contains(&name.as_str()))
                .cloned()
                .collect()
        }
    }
}

fn variant_tag(variant: &Value) -> String {
    literal(&variant["properties"][TAG]["const"])
}

/// Which sockets carry each message variant, keyed by direction, type and tag.
fn carried(sockets: &[CallSocket]) -> BTreeMap<(Direction, &str, String), Vec<&str>> {
    let mut carried: BTreeMap<(Direction, &str, String), Vec<&str>> = BTreeMap::new();
    for socket in sockets {
        for (direction, messages) in [
            (Direction::Client, socket.client),
            (Direction::Server, socket.server),
        ] {
            for message in messages {
                let root = root_schema(message.type_name, direction);
                let tags: Vec<String> = root["oneOf"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{} is not a tagged enum", message.type_name))
                    .iter()
                    .map(variant_tag)
                    .collect();
                for tag in selected_tags(message, &tags) {
                    carried
                        .entry((direction, message.type_name, tag))
                        .or_default()
                        .push(socket.name);
                }
            }
        }
    }
    carried
}

fn message_rows(sockets: &[CallSocket]) -> Vec<Row> {
    let carried = carried(sockets);
    let mut rows = Vec::new();
    for ((direction, type_name, tag), names) in carried {
        let root = root_schema(type_name, direction);
        let defs = root
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let variant = root["oneOf"]
            .as_array()
            .and_then(|variants| variants.iter().find(|v| variant_tag(v) == tag))
            .expect("variant exists");
        let mut walker = Walker::new(&defs, direction);
        let shape = walker.normalize(variant);
        let item = format!("{} {type_name}[{tag}]", direction.label());
        let mut attributes = vec![
            "message".to_owned(),
            format!(
                "{} {}",
                if names.len() == 1 {
                    "socket"
                } else {
                    "sockets"
                },
                names.join("|")
            ),
        ];
        if direction == Direction::Client
            && shape.schema.get("additionalProperties") == Some(&Value::Bool(false))
        {
            attributes.push("rejects unknown fields".into());
        }
        walker.rows.push(Row {
            item: item.clone(),
            attributes,
            doc: describe(shape.schema.get("description")),
        });
        let required = required_keys(&shape.schema);
        if let Some(Value::Object(properties)) = shape.schema.get("properties") {
            for (key, property) in properties.iter().filter(|(key, _)| key.as_str() != TAG) {
                walker.walk(
                    format!("{item}.{key}"),
                    property,
                    required.iter().any(|name| name == key),
                    None,
                );
            }
        }
        rows.extend(walker.rows);
    }
    rows
}

fn socket_rows(sockets: &[CallSocket]) -> Vec<Row> {
    sockets
        .iter()
        .map(|socket| Row {
            item: format!("socket {}", socket.name),
            attributes: vec![
                format!("path {}", socket.path),
                format!("scope {}", socket.scope),
                format!("protocol {}", socket.protocol_version),
            ],
            doc: socket.doc.to_owned(),
        })
        .collect()
}

fn path_parameter_doc(name: &str) -> &'static str {
    match name {
        "a" => "Account ID or alias.",
        "call" => "ID of the call whose audio the socket carries.",
        other => panic!("describe the {{{other}}} path parameter of the call sockets"),
    }
}

/// The AsyncAPI 3.0 description of the sockets, from the same table and
/// schemas as the listing. The daemon is the application: it receives the
/// messages clients send and sends the events and audio clients receive.
fn asyncapi() -> Value {
    let carried = carried(SOCKETS);
    let mut components = Components::new();
    for (direction, type_name, tag) in carried.keys() {
        let root = root_schema(type_name, *direction);
        let variant = root["oneOf"]
            .as_array()
            .and_then(|variants| variants.iter().find(|v| variant_tag(v) == *tag))
            .expect("variant exists");
        let mut payload = variant.clone();
        if let Some(defs) = root.get("$defs") {
            payload["$defs"] = defs.clone();
        }
        components.add(*direction, &format!("{type_name}.{tag}"), &payload);
    }
    let mut schemas = components.schemas("#/components/schemas/");
    schemas.values_mut().for_each(collapse_single_alternatives);
    let key = |direction: Direction, tag: &str| format!("{}.{tag}", direction.label());
    let mut messages = Map::new();
    for (direction, type_name, tag) in carried.keys() {
        let root = root_schema(type_name, *direction);
        let variant = root["oneOf"]
            .as_array()
            .and_then(|variants| variants.iter().find(|v| variant_tag(v) == *tag))
            .expect("variant exists");
        messages.insert(
            key(*direction, tag),
            json!({
                "name": tag,
                "title": format!("{type_name}[{tag}]"),
                "summary": describe(variant.get("description")),
                "contentType": "application/json",
                "payload": {"$ref": format!(
                    "#/components/schemas/{}",
                    components.name(*direction, &format!("{type_name}.{tag}"))
                )},
            }),
        );
    }
    let mut channels = Map::new();
    let mut operations = Map::new();
    for socket in SOCKETS {
        let mut channel_messages = Map::new();
        let mut action_messages: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for ((direction, _, tag), names) in &carried {
            if !names.contains(&socket.name) {
                continue;
            }
            let id = key(*direction, tag);
            channel_messages.insert(
                id.clone(),
                json!({"$ref": format!("#/components/messages/{id}")}),
            );
            let action = if *direction == Direction::Client {
                "receive"
            } else {
                "send"
            };
            action_messages
                .entry(action)
                .or_default()
                .push(json!({"$ref": format!("#/channels/{}/messages/{id}", socket.name)}));
        }
        let parameters: Map<String, Value> = socket
            .path
            .split('/')
            .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
            .map(|name| {
                (
                    name.to_owned(),
                    json!({"description": path_parameter_doc(name)}),
                )
            })
            .collect();
        channels.insert(
            socket.name.to_owned(),
            json!({
                "address": socket.path,
                "title": format!("The {} socket", socket.name),
                "description": socket.doc,
                "parameters": parameters,
                "messages": channel_messages,
            }),
        );
        for (action, messages) in action_messages {
            let summary = if action == "receive" {
                "Messages the daemon accepts from the client."
            } else {
                "Messages the daemon sends to the client."
            };
            operations.insert(
                format!("{}{}", socket.name, if action == "receive" { "Receive" } else { "Send" }),
                json!({
                    "action": action,
                    "channel": {"$ref": format!("#/channels/{}", socket.name)},
                    "summary": summary,
                    "messages": messages,
                    "security": [{
                        "type": "http",
                        "scheme": "bearer",
                        "x-agent-wechat-scope": socket.scope,
                        "description": format!("An API token with the {} scope, sent in the Authorization header of the upgrade request.", socket.scope),
                    }],
                }),
            );
        }
    }
    let protocol = SOCKETS[0].protocol_version;
    json!({
        "asyncapi": "3.0.0",
        "info": {
            "title": "agent-wechat calls WebSocket API",
            "version": protocol.to_string(),
            "description": format!("Call commands, events and audio on two WebSockets of the agent-wechat daemon. Both speak call protocol version {protocol}. The daemon's HTTP routes are described in openapi.json."),
            "license": {"name": env!("CARGO_PKG_LICENSE")},
        },
        "servers": {"daemon": {
            "host": "127.0.0.1:6174",
            "protocol": "ws",
            "description": "The daemon listens on localhost by default.",
        }},
        "channels": channels,
        "operations": operations,
        "components": {
            "messages": messages,
            "schemas": schemas,
        },
    })
}

fn listing() -> String {
    let mut rows = socket_rows(SOCKETS);
    rows.extend(message_rows(SOCKETS));
    // Sort by item so a message's row precedes its fields.
    rows.sort_by(|a, b| a.item.cmp(&b.item));
    let mut text = HEADER.to_owned();
    for row in rows {
        text.push_str(&row.render());
        text.push('\n');
    }
    text
}

fn listing_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../interfaces/http/calls-websocket.txt")
}

#[test]
fn calls_websocket_listing_is_current() {
    let expected = listing();
    let path = listing_path();
    if std::env::var_os(UPDATE).is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &expected).unwrap();
        return;
    }
    let actual = fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if actual != expected {
        let old = actual.lines().collect::<std::collections::BTreeSet<_>>();
        let new = expected.lines().collect::<std::collections::BTreeSet<_>>();
        let changes = old
            .difference(&new)
            .map(|line| format!("- {line}"))
            .chain(new.difference(&old).map(|line| format!("+ {line}")))
            .take(40)
            .collect::<Vec<_>>()
            .join("\n");
        panic!("interfaces/http/calls-websocket.txt does not match the call messages. Run `pnpm interfaces` and review the diff:\n{changes}");
    }
}

#[test]
fn calls_interface_asyncapi_is_current() {
    check_file("asyncapi.json", &to_json_text(&asyncapi()));
}

#[test]
fn calls_websocket_listing_rows_have_unique_sorted_items() {
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
fn calls_websocket_listing_lists_each_direction_separately() {
    let listing = listing();
    for expected in [
        "client CallClientHello[call.hello] | ",
        "client CallCommand[call.dial] | ",
        "client CallMediaCommand[media.attach] | ",
        "server CallEvent[hello] | ",
        "server CallEvent[media.ready] | ",
        "server CallMediaCommand[audio.chunk] | ",
    ] {
        assert!(
            listing.lines().any(|line| line.starts_with(expected)),
            "missing row {expected}"
        );
    }
    // The daemon never sends these on the media socket, and never receives events.
    for absent in [
        "server CallMediaCommand[media.attach]",
        "server CallMediaCommand[audio.start]",
        "client CallEvent[",
        "server CallEvent[media.ready] | message, socket control",
    ] {
        assert!(!listing.contains(absent), "unexpected row {absent}");
    }
}

/// The messages use `deny_unknown_fields`, so the schemas must say so and the
/// listing must pass it on to clients.
#[test]
fn calls_websocket_client_messages_reject_unknown_fields() {
    let listing = listing();
    let messages: Vec<_> = listing
        .lines()
        .filter(|line| line.starts_with("client ") && line.contains(" | message, "))
        .collect();
    assert!(messages.len() >= 13, "expected every client message");
    for line in messages {
        assert!(line.contains("rejects unknown fields"), "{line}");
    }
}

const OWNER: &str = "synthetic-owner-api-token-for-call-listing-tests";

/// The socket table is only trustworthy if the router really serves each path
/// for its scope, so upgrade every table entry against the real router.
#[tokio::test]
async fn socket_table_matches_the_router() {
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Error as SocketError},
    };
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let core = Core::new(store, None).unwrap();
    let grant = |token: &str, scope: &str| {
        use sha2::{Digest, Sha256};
        core.store
            .issue_token(
                "call-listing",
                &format!("{:x}", Sha256::digest(token)),
                &["one".to_owned()],
                &[scope.to_owned()],
                None,
            )
            .unwrap();
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = router(core.clone(), OWNER);
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    // Resolves to the upgrade status: 101 when the socket opened.
    let upgrade = |path: String, token: &'static str| async move {
        let mut request = format!("ws://{address}{path}")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        match connect_async(request).await {
            Ok((_, response)) => response.status().as_u16(),
            Err(SocketError::Http(response)) => response.status().as_u16(),
            Err(other) => panic!("unexpected socket error {other}"),
        }
    };
    let fill = |path: &str| path.replace("{a}", "one").replace("{call}", "example-call");
    grant("synthetic-wrong-scope-token-for-call-listing", "read");
    for socket in SOCKETS {
        let path = fill(socket.path);
        assert_eq!(
            upgrade(path.clone(), "synthetic-wrong-scope-token-for-call-listing").await,
            403,
            "{} must require a token with another scope than read",
            socket.name
        );
        grant("synthetic-right-scope-token-for-call-listing", socket.scope);
        assert_eq!(
            upgrade(path, "synthetic-right-scope-token-for-call-listing").await,
            101,
            "{} should open at {} for scope {}",
            socket.name,
            socket.path,
            socket.scope
        );
    }
    // A path the router does not serve must fail, or the checks above prove nothing.
    assert_eq!(
        upgrade(fill("/v1/accounts/{a}/calls/not-a-socket"), OWNER).await,
        404
    );
    server.abort();
}
