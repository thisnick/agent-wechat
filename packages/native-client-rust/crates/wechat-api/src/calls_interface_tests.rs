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
};
use schemars::generate::{Contract, SchemaSettings};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};
use wechat_core::{
    call_types::{CallClientHello, CallCommand, CallEvent, CallMediaCommand},
    Core,
};
use wechat_store::Store;

const UPDATE: &str = "AGENT_WECHAT_UPDATE_INTERFACES";
const MISSING: &str = "MISSING DESCRIPTION";
const TAG: &str = "type";
const HEADER: &str = "# Calls WebSocket protocol. Generated from the message types and socket table in crates/wechat-api; do not edit.\n# Run `pnpm interfaces` after changing the call messages or sockets. Format: ITEM | ATTRIBUTES | DOC\n# `client` messages are sent to the daemon, `server` messages come from it. Message items are TYPE[tag], the value of `type`; nested fields use dots and `[]` marks array items.\n# `when kind x` limits a field to one variant of a nested message. `nullable` means JSON null is allowed. Server rows describe what the daemon sends, client rows what it accepts.\n";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Direction {
    Client,
    Server,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
    fn contract(self) -> Contract {
        match self {
            Self::Client => Contract::Deserialize,
            Self::Server => Contract::Serialize,
        }
    }
}

struct Row {
    item: String,
    attributes: Vec<String>,
    doc: String,
}

impl Row {
    fn render(&self) -> String {
        let attributes = if self.attributes.is_empty() {
            "-".to_owned()
        } else {
            self.attributes.join(", ")
        };
        for text in [&self.item, &attributes, &self.doc] {
            assert!(
                !text.contains(" | ") && !text.contains('\n'),
                "listing cell must stay on one line without ' | ': {text}"
            );
        }
        format!("{} | {attributes} | {}", self.item, self.doc)
    }
}

/// Collapses a doc comment to one line.
fn describe(text: Option<&Value>) -> String {
    text.and_then(Value::as_str)
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| MISSING.to_owned())
}

fn literal(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn is_null_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

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

/// A schema with references, nullability and sibling keywords folded in.
struct Shape {
    schema: Map<String, Value>,
    nullable: bool,
    /// Set when the schema refers to a definition that is already being listed.
    cycle: Option<String>,
}

/// Flattens one message type's schema into rows.
struct Walker<'a> {
    defs: &'a Map<String, Value>,
    direction: Direction,
    /// Definitions being expanded, to stop a recursive type.
    stack: Vec<String>,
    rows: Vec<Row>,
}

impl<'a> Walker<'a> {
    fn new(defs: &'a Map<String, Value>, direction: Direction) -> Self {
        Self {
            defs,
            direction,
            stack: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Resolves `$ref`, unwraps `Option`-style nullability and merges sibling
    /// keywords over the referenced definition, so a field's own doc comment
    /// replaces the doc comment of its type.
    fn normalize(&mut self, schema: &Value) -> Shape {
        let mut current = schema.as_object().cloned().unwrap_or_default();
        let mut nullable = false;
        loop {
            if let Some(reference) = current.remove("$ref") {
                let name = reference
                    .as_str()
                    .and_then(|reference| reference.strip_prefix("#/$defs/"))
                    .unwrap_or_else(|| panic!("unsupported schema reference {reference}"))
                    .to_owned();
                if self.stack.contains(&name) {
                    return Shape {
                        schema: current,
                        nullable,
                        cycle: Some(name),
                    };
                }
                let mut base = self
                    .defs
                    .get(&name)
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| panic!("schema has no definition {name}"))
                    .clone();
                self.stack.push(name);
                base.extend(current);
                current = base;
                continue;
            }
            if let Some(Value::Array(branches)) = current.get("anyOf") {
                let others: Vec<&Value> = branches.iter().filter(|b| !is_null_schema(b)).collect();
                if others.len() == 1 && others.len() < branches.len() {
                    nullable = true;
                    let mut base = others[0].as_object().cloned().unwrap_or_default();
                    current.remove("anyOf");
                    base.extend(current);
                    current = base;
                    continue;
                }
            }
            if let Some(Value::Array(types)) = current.get("type") {
                let kept: Vec<Value> = types
                    .iter()
                    .filter(|t| t.as_str() != Some("null"))
                    .cloned()
                    .collect();
                if kept.len() < types.len() {
                    nullable = true;
                    let replacement = if kept.len() == 1 {
                        kept[0].clone()
                    } else {
                        Value::Array(kept)
                    };
                    current.insert("type".into(), replacement);
                }
            }
            return Shape {
                schema: current,
                nullable,
                cycle: None,
            };
        }
    }

    fn walk(&mut self, item: String, schema: &Value, required: bool, when: Option<String>) {
        let depth = self.stack.len();
        let shape = self.normalize(schema);
        if let Some(name) = &shape.cycle {
            let mut attributes = vec!["object".to_owned()];
            attributes.extend(required.then(|| "required".to_owned()));
            attributes.push(format!("recursive, see {name}"));
            self.rows.push(Row {
                item,
                attributes,
                doc: describe(shape.schema.get("description")),
            });
        } else {
            self.emit(item, &shape, required, when);
        }
        self.stack.truncate(depth);
    }

    fn type_label(schema: &Map<String, Value>) -> String {
        let from_branches = || {
            schema
                .get("oneOf")
                .and_then(Value::as_array)
                .and_then(|branches| branches.first())
                .and_then(|branch| branch.get("type"))
        };
        match schema.get("type").or_else(from_branches) {
            Some(Value::String(kind)) => kind.clone(),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|"),
            _ => "any".to_owned(),
        }
    }

    /// Attributes shared by every kind of value.
    fn attributes(
        &self,
        schema: &Map<String, Value>,
        nullable: bool,
        required: bool,
        when: Option<String>,
    ) -> Vec<String> {
        let mut attributes = vec![Self::type_label(schema)];
        if required {
            attributes.push("required".into());
        }
        if nullable {
            attributes.push("nullable".into());
        }
        if let Some(Value::Array(values)) = schema.get("enum") {
            let values: Vec<_> = values.iter().map(literal).collect();
            attributes.push(format!("values {}", values.join("|")));
        }
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            if branches.iter().all(|branch| branch.get("const").is_some()) {
                let values: Vec<_> = branches.iter().map(|b| literal(&b["const"])).collect();
                attributes.push(format!("values {}", values.join("|")));
            }
        }
        if let Some(value) = schema.get("const") {
            attributes.push(format!("const {}", literal(value)));
        }
        for (key, label) in [
            ("minimum", "min"),
            ("maximum", "max"),
            ("minLength", "min length"),
            ("maxLength", "max length"),
            ("minItems", "min items"),
            ("maxItems", "max items"),
            ("pattern", "pattern"),
            ("format", "format"),
        ] {
            if let Some(value) = schema.get(key) {
                attributes.push(format!("{label} {}", literal(value)));
            }
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            attributes.push("unique items".into());
        }
        // The daemon always sends a field it serializes, so a default only
        // tells a client what omitting the field means.
        if self.direction == Direction::Client {
            if let Some(value) = schema.get("default") {
                attributes.push(format!("default {}", literal(value)));
            }
        }
        if self.direction == Direction::Client
            && schema.get("additionalProperties") == Some(&Value::Bool(false))
        {
            attributes.push("rejects unknown fields".into());
        }
        attributes.extend(when);
        attributes
    }

    fn emit(&mut self, item: String, shape: &Shape, required: bool, when: Option<String>) {
        let schema = &shape.schema;
        let doc = describe(schema.get("description"));
        let mut attributes = self.attributes(schema, shape.nullable, required, when);
        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            if !branches.iter().all(|branch| branch.get("const").is_some()) {
                self.rows.push(Row {
                    item: item.clone(),
                    attributes,
                    doc,
                });
                self.union(&item, branches);
                return;
            }
        }
        if schema.get("type").and_then(Value::as_str) == Some("array") {
            let items = schema.get("items").cloned().unwrap_or(Value::Bool(true));
            let depth = self.stack.len();
            let element = self.normalize(&items);
            let nested = element.cycle.is_none()
                && (element.schema.contains_key("properties")
                    || element.schema.contains_key("oneOf"));
            let label = Self::type_label(&element.schema);
            attributes[0] = format!("array of {label}");
            if !nested {
                let mut element_attributes = self.attributes(&element.schema, false, false, None);
                // The first entry repeats the type label already in the array's own.
                element_attributes.remove(0);
                attributes.extend(
                    element_attributes
                        .into_iter()
                        .map(|attribute| format!("items {attribute}")),
                );
            }
            self.rows.push(Row {
                item: item.clone(),
                attributes,
                doc,
            });
            if nested {
                self.emit(format!("{item}[]"), &element, false, None);
            }
            self.stack.truncate(depth);
            return;
        }
        self.rows.push(Row {
            item: item.clone(),
            attributes,
            doc,
        });
        self.properties(&item, schema);
    }

    /// Rows for the properties of an object schema.
    fn properties(&mut self, item: &str, schema: &Map<String, Value>) {
        let Some(Value::Object(properties)) = schema.get("properties") else {
            return;
        };
        let required = required_keys(schema);
        for (key, property) in properties {
            self.walk(
                format!("{item}.{key}"),
                property,
                required.iter().any(|name| name == key),
                None,
            );
        }
    }

    /// Rows for a nested message with several variants. The tag becomes one
    /// field listing every value; other fields say which variants carry them.
    fn union(&mut self, item: &str, branches: &[Value]) {
        let depth = self.stack.len();
        let shapes: Vec<Shape> = branches.iter().map(|b| self.normalize(b)).collect();
        let objects: Vec<&Map<String, Value>> = shapes.iter().map(|shape| &shape.schema).collect();
        let properties = |object: &Map<String, Value>| {
            object
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        let tag = properties(objects[0])
            .into_iter()
            .find(|(key, _)| {
                objects.iter().all(|object| {
                    properties(object)
                        .get(key)
                        .is_some_and(|p| p.get("const").is_some())
                })
            })
            .map(|(key, _)| key)
            .unwrap_or_else(|| panic!("{item} is a union without a tag field"));
        let tag_value = |object: &Map<String, Value>| literal(&properties(object)[&tag]["const"]);
        let mut keys: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, object) in objects.iter().enumerate() {
            for key in properties(object).keys() {
                keys.entry(key.clone()).or_default().push(index);
            }
        }
        for (key, owners) in keys {
            let field = format!("{item}.{key}");
            let owner_values: Vec<String> = owners.iter().map(|&i| tag_value(objects[i])).collect();
            if key == tag {
                let docs: Vec<String> = owners
                    .iter()
                    .map(|&i| {
                        let doc = describe(objects[i].get("description"));
                        format!("{}: {}", tag_value(objects[i]), doc.trim_end_matches('.'))
                    })
                    .collect();
                let doc = if docs.iter().any(|doc| doc.contains(MISSING)) {
                    MISSING.to_owned()
                } else {
                    docs.join("; ")
                };
                self.rows.push(Row {
                    item: field,
                    attributes: vec![
                        "string".into(),
                        "required".into(),
                        format!("values {}", owner_values.join("|")),
                    ],
                    doc,
                });
                continue;
            }
            let first = properties(objects[owners[0]])[&key].clone();
            for &i in &owners[1..] {
                let other = properties(objects[i])[&key].clone();
                assert_eq!(
                    first, other,
                    "{field} differs between variants; the listing merges them"
                );
            }
            let all_required = owners
                .iter()
                .all(|&i| required_keys(objects[i]).iter().any(|name| name == &key));
            let when = (owners.len() < objects.len())
                .then(|| format!("when {tag} {}", owner_values.join("|")));
            self.walk(field, &first, all_required, when);
        }
        self.stack.truncate(depth);
    }
}

fn required_keys(schema: &Map<String, Value>) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
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

fn message_rows(sockets: &[CallSocket]) -> Vec<Row> {
    // Which sockets carry each message variant, keyed by direction, type and tag.
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
