use super::wechat_db::{get_db_path, query_wechat_db};
use crate::ia::types::{Message, PaymentInfo, ReplyInfo};
use md5::{Digest, Md5};
use std::collections::{HashMap, HashSet};

/// ZSTD magic number (little-endian): 0xFD2FB528
const ZSTD_MAGIC: &str = "28b52ffd";

/// Get the Msg table name for a given chat username.
/// WeChat uses MD5(username) as the table suffix.
pub fn get_msg_table_name(chat_id: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(chat_id.as_bytes());
    let hash = hasher.finalize();
    format!("Msg_{:x}", hash)
}

/// Decode hex-encoded message content, decompressing zstd if needed.
pub fn decode_message_content(hex: &str, is_compressed: bool) -> String {
    if hex.is_empty() {
        return String::new();
    }
    let bytes = match hex_decode(hex) {
        Some(b) => b,
        None => return String::new(),
    };
    if is_compressed && hex.len() >= 8 && hex[..8].eq_ignore_ascii_case(ZSTD_MAGIC) {
        match zstd::decode_all(bytes.as_slice()) {
            Ok(decompressed) => String::from_utf8_lossy(&decompressed).to_string(),
            Err(_) => "[compressed content - decompression failed]".to_string(),
        }
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    }
}

/// Decode a hex string to bytes.
pub fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

/// Extract sender from group message content.
/// Group messages have format "sender_wxid:\nmessage_content".
fn extract_group_sender(content: &str) -> (Option<String>, String) {
    if let Some(idx) = content.find(":\n") {
        if idx < 80 {
            let sender = content[..idx].to_string();
            let msg = content[idx + 2..].to_string();
            return (Some(sender), msg);
        }
    }
    (None, content.to_string())
}

/// Clean message content for display based on message type.
/// Replaces verbose XML with concise summaries.
fn clean_content(content: &str, msg_type: i32) -> String {
    let base = msg_type & 0x7FFFFFFF;
    match base {
        // Image (type 3): replace XML with empty string
        3 if content.contains("<img") => String::new(),
        // Emoji (type 47): show cdnurl or [emoji]
        47 if content.contains("<emoji") => extract_xml_attr(content, "cdnurl")
            .filter(|u| u.starts_with("http"))
            .unwrap_or_else(|| "[emoji]".to_string()),
        // Appmsg (type 49): handle subtypes
        49 if content.contains("<msg>") => {
            let title = extract_xml_tag(content, "title").unwrap_or_default();
            let appmsg_type = extract_appmsg_type(content).unwrap_or(0);
            match appmsg_type {
                2000 => {
                    let amount = extract_xml_tag(content, "feedesc").unwrap_or_default();
                    if title.is_empty() {
                        amount
                    } else if amount.is_empty() {
                        title
                    } else {
                        format!("{title} {amount}")
                    }
                }
                // Link share (5), video link (4), music share (3)
                3 | 4 | 5 => {
                    let mut parts = Vec::new();
                    parts.push(format!("[Link] {title}"));
                    if let Some(des) = extract_xml_tag(content, "des") {
                        parts.push(des);
                    }
                    if let Some(url) = extract_xml_tag(content, "url") {
                        let url = url.replace("&amp;", "&");
                        parts.push(url);
                    }
                    parts.join("\n")
                }
                _ => {
                    if title.is_empty() {
                        content.to_string()
                    } else {
                        title
                    }
                }
            }
        }
        _ => content.to_string(),
    }
}

/// Extract reply info from type 49 (appmsg) messages with <refermsg>.
fn extract_reply_info(content: &str, msg_type: i32) -> Option<ReplyInfo> {
    let base = msg_type & 0x7FFFFFFF;
    if base != 49 || !content.contains("<refermsg>") {
        return None;
    }
    // Extract the refermsg block
    let rm_start = content.find("<refermsg>")?;
    let rm_end = content.find("</refermsg>")? + "</refermsg>".len();
    let refermsg = &content[rm_start..rm_end];

    let sender = extract_xml_tag(refermsg, "displayname");
    let ref_content = extract_xml_tag(refermsg, "content").unwrap_or_default();

    // The referred content may be XML-escaped — unescape first
    let unescaped = ref_content
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"");

    // The referred content may itself be XML — clean it to a short text
    let clean = if unescaped.contains("<msg>") {
        extract_xml_tag(&unescaped, "title").unwrap_or(unescaped)
    } else {
        unescaped
    };
    Some(ReplyInfo {
        sender,
        content: clean,
    })
}

/// Extract an XML attribute value: attr="value"
fn extract_xml_attr(xml: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}=\"");
    let start = xml.find(&pattern)? + pattern.len();
    let end = xml[start..].find('"')? + start;
    let val = xml[start..end].trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

/// Extract text between XML tags: <tag>text</tag>
/// Also handles CDATA: <tag><![CDATA[text]]></tag>
pub(crate) fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let mut val = xml[start..end].trim().to_string();
    // Strip CDATA wrapper if present
    if val.starts_with("<![CDATA[") && val.ends_with("]]>") {
        val = val[9..val.len() - 3].to_string();
    }
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next()?.trim();
        let v = parts.next()?.trim();
        if k == key && !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

fn extract_appmsg_type(content: &str) -> Option<i32> {
    extract_xml_tag(content, "type").and_then(|t| t.parse::<i32>().ok())
}

fn parse_amount_cents(text: &str) -> Option<i64> {
    let filtered: String = text
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if filtered.is_empty() {
        return None;
    }

    let negative = filtered.starts_with('-');
    let unsigned = filtered.trim_start_matches('-');
    let mut parts = unsigned.splitn(2, '.');
    let whole = parts.next()?.parse::<i64>().ok()?;
    let frac_raw = parts.next().unwrap_or("");
    let mut frac = frac_raw.chars().take(2).collect::<String>();
    while frac.len() < 2 {
        frac.push('0');
    }
    let frac = frac.parse::<i64>().ok()?;
    let cents = whole.checked_mul(100)?.checked_add(frac)?;
    Some(if negative { -cents } else { cents })
}

fn extract_payment_info(content: &str, msg_type: i32) -> Option<PaymentInfo> {
    let base = msg_type & 0x7FFFFFFF;
    if base != 49 || !content.contains("<msg>") {
        return None;
    }

    let app_msg_type = extract_appmsg_type(content)?;
    match app_msg_type {
        2000 => {
            let amount_text = extract_xml_tag(content, "feedesc");
            Some(PaymentInfo {
                kind: "transfer".to_string(),
                app_msg_type: Some(app_msg_type),
                amount_cents: amount_text.as_deref().and_then(parse_amount_cents),
                amount_text,
                currency: Some("CNY".to_string()),
                transaction_id: extract_xml_tag(content, "transcationid"),
                transfer_id: extract_xml_tag(content, "transferid"),
                send_id: None,
                pay_msg_id: None,
                receiver_title: None,
                native_url: None,
            })
        }
        2001 => {
            let message_url = extract_xml_tag(content, "url");
            let native_url = extract_xml_tag(content, "nativeurl");
            let send_id = native_url
                .as_deref()
                .and_then(|u| extract_query_param(u, "sendid"))
                .or_else(|| {
                    message_url
                        .as_deref()
                        .and_then(|u| extract_query_param(u, "sendid"))
                });

            Some(PaymentInfo {
                kind: "red_packet".to_string(),
                app_msg_type: Some(app_msg_type),
                amount_text: None,
                amount_cents: None,
                currency: Some("CNY".to_string()),
                transaction_id: None,
                transfer_id: None,
                send_id,
                pay_msg_id: extract_xml_tag(content, "paymsgid"),
                receiver_title: extract_xml_tag(content, "receivertitle"),
                native_url,
            })
        }
        _ => None,
    }
}

fn message_kind(msg_type: i32, app_msg_type: Option<i32>, payment: Option<&PaymentInfo>) -> String {
    if let Some(payment) = payment {
        return payment.kind.clone();
    }

    match msg_type & 0x7FFFFFFF {
        1 => "text",
        3 => "image",
        34 => "voice",
        42 => "contact",
        43 | 62 => "video",
        47 => "emoji",
        48 => "location",
        49 => match app_msg_type {
            Some(3) => "music",
            Some(4) | Some(5) => "link",
            Some(6) => "file",
            Some(8) => "sticker",
            Some(19) => "location",
            Some(33) | Some(36) => "mini_program",
            Some(57) => "reply",
            Some(63) => "livestream",
            _ => "app",
        },
        10000 => "system",
        10002 => "recalled",
        _ => "unknown",
    }
    .to_string()
}

fn payment_identity(payment: &PaymentInfo) -> Option<&str> {
    payment
        .transaction_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .or_else(|| payment.transfer_id.as_deref().filter(|id| !id.is_empty()))
}

fn infer_payment_receipt_state(messages: &mut [Message]) {
    let self_transfer_ids: HashSet<String> = messages
        .iter()
        .filter(|message| message.is_self == Some(true))
        .filter_map(|message| match message.payment.as_ref() {
            Some(payment) if payment.kind == "transfer" => {
                payment_identity(payment).map(str::to_string)
            }
            _ => None,
        })
        .collect();
    let inbound_transfer_ids: HashSet<String> = messages
        .iter()
        .filter(|message| message.is_self != Some(true))
        .filter_map(|message| match message.payment.as_ref() {
            Some(payment) if payment.kind == "transfer" => {
                payment_identity(payment).map(str::to_string)
            }
            _ => None,
        })
        .collect();
    let received_transfer_ids: HashSet<String> = self_transfer_ids
        .intersection(&inbound_transfer_ids)
        .cloned()
        .collect();

    if received_transfer_ids.is_empty() {
        return;
    }

    for message in messages {
        let Some(payment) = message.payment.as_ref() else {
            continue;
        };
        if payment.kind != "transfer" {
            continue;
        }
        if payment_identity(payment).is_some_and(|id| received_transfer_ids.contains(id)) {
            message.is_received = Some(true);
        }
    }
}

/// Check if the source XML indicates the current user is @-mentioned.
fn check_is_mentioned(source: &str, account_dir: &str) -> bool {
    if let Some(at_list) = extract_xml_tag(source, "atuserlist") {
        // atuserlist may contain comma-separated wxids
        for wxid in at_list.split(',') {
            let wxid = wxid.trim();
            if !wxid.is_empty() && account_dir.starts_with(wxid) {
                return true;
            }
        }
    }
    false
}

/// Find which message DB contains a chat and return (db_name, key).
pub fn find_message_db<'a>(
    account_dir: &str,
    keys: &'a HashMap<String, String>,
    chat_id: &str,
) -> Option<(String, &'a str)> {
    let table_name = get_msg_table_name(chat_id);
    let mut message_dbs: Vec<(&str, &str)> = keys
        .iter()
        .filter(|(k, _)| {
            k.starts_with("message_")
                && k.ends_with(".db")
                && !k.contains("fts")
                && !k.contains("resource")
        })
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    message_dbs.sort_by_key(|(k, _)| k.to_string());

    for (db_name, key) in &message_dbs {
        let db_path = get_db_path(account_dir, db_name);
        let check = query_wechat_db(
            &db_path,
            key,
            &format!("SELECT name FROM sqlite_master WHERE type='table' AND name='{table_name}';"),
        );
        if !check.is_empty() {
            return Some((db_name.to_string(), key));
        }
    }
    None
}

/// List messages for a specific chat.
///
/// Messages may be spread across message_0.db, message_1.db, etc.
/// Each chat's messages are in a `Msg_{MD5(username)}` table.
pub fn list_messages(
    account_dir: &str,
    keys: &HashMap<String, String>,
    chat_id: &str,
    limit: i64,
    offset: i64,
) -> Vec<Message> {
    let table_name = get_msg_table_name(chat_id);
    let is_group = chat_id.contains("@chatroom");

    let (db_name, key) = match find_message_db(account_dir, keys, chat_id) {
        Some(dk) => dk,
        None => return Vec::new(),
    };
    let db_path = get_db_path(account_dir, &db_name);

    // Query messages using hex() for safe binary/compressed content extraction
    let rows = query_wechat_db(
        &db_path,
        key,
        &format!(
            "SELECT m.local_id, m.server_id, m.local_type, m.create_time,
                    hex(m.message_content) as hex_content,
                    m.WCDB_CT_message_content as is_compressed,
                    hex(m.source) as hex_source,
                    m.WCDB_CT_source as source_compressed,
                    n.user_name as sender_name
             FROM \"{table_name}\" m
             LEFT JOIN Name2Id n ON m.real_sender_id = n.rowid
             ORDER BY m.create_time DESC
             LIMIT {limit} OFFSET {offset};"
        ),
    );

    // Resolve sender display names from contact.db
    let contact_names: HashMap<String, String> = {
        let mut map = HashMap::new();
        if let Some(contact_key) = keys.get("contact.db") {
            // Collect unique sender wxids
            let senders: Vec<String> = rows
                .iter()
                .filter_map(|row| {
                    row.get("sender_name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            if !senders.is_empty() {
                let contact_db = get_db_path(account_dir, "contact.db");
                let placeholders = senders
                    .iter()
                    .map(|s| format!("'{}'", s.replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(",");
                let contacts = query_wechat_db(
                    &contact_db,
                    contact_key,
                    &format!("SELECT username, nick_name, remark FROM contact WHERE username IN ({placeholders});"),
                );
                for c in contacts {
                    if let Some(username) = c.get("username").and_then(|v| v.as_str()) {
                        let name = c
                            .get("remark")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .or_else(|| {
                                c.get("nick_name")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                            })
                            .unwrap_or(username);
                        map.insert(username.to_string(), name.to_string());
                    }
                }
            }
        }
        map
    };

    let mut messages: Vec<Message> = rows
        .iter()
        .filter_map(|row| {
            let local_id = row.get("local_id")?.as_i64()?;
            let server_id = row.get("server_id").and_then(|v| v.as_i64()).unwrap_or(0);
            let msg_type = row.get("local_type").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

            let hex_content = row
                .get("hex_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let is_compressed = row
                .get("is_compressed")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                != 0;

            let raw_content = decode_message_content(hex_content, is_compressed);

            // Get sender from Name2Id join (works for both group and 1:1 chats)
            let sender = row
                .get("sender_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            // Strip group sender prefix from content ("wxid:\ncontent" format)
            let body = if is_group {
                extract_group_sender(&raw_content).1
            } else {
                raw_content
            };

            // Extract reply info before cleaning (needs raw XML)
            let reply = extract_reply_info(&body, msg_type);
            let app_msg_type = extract_appmsg_type(&body);
            let payment = extract_payment_info(&body, msg_type);
            let kind = message_kind(msg_type, app_msg_type, payment.as_ref());
            let is_received = payment.as_ref().map(|_| false);

            // Clean content for display (replace XML with summaries)
            let content = clean_content(&body, msg_type);

            let timestamp = row
                .get("create_time")
                .and_then(|v| v.as_i64())
                .map(|t| {
                    chrono::DateTime::from_timestamp(t, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            // Check @-mention from source XML (only for group chats)
            let is_mentioned = if is_group {
                let hex_source = row.get("hex_source").and_then(|v| v.as_str()).unwrap_or("");
                let source_compressed = row
                    .get("source_compressed")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    != 0;
                let from_source = if !hex_source.is_empty() {
                    let source_xml = decode_message_content(hex_source, source_compressed);
                    check_is_mentioned(&source_xml, account_dir)
                } else {
                    false
                };

                // For type-49 (appmsg) messages — especially reference/quote messages —
                // WeChat may place <atuserlist> inside the content XML instead of source.
                let from_content = if !from_source && (msg_type & 0x7FFFFFFF) == 49 {
                    check_is_mentioned(&body, account_dir)
                } else {
                    false
                };

                if from_source || from_content {
                    Some(true)
                } else {
                    None
                }
            } else {
                None
            };

            // Check if message was sent by the logged-in user
            let is_self = sender.as_ref().map(|s| account_dir.starts_with(s.as_str()));

            let sender_name = sender
                .as_ref()
                .and_then(|wxid| contact_names.get(wxid))
                .cloned();

            Some(Message {
                local_id,
                server_id,
                chat_id: chat_id.to_string(),
                sender,
                sender_name,
                msg_type,
                kind,
                app_msg_type,
                content,
                timestamp,
                is_mentioned,
                is_self,
                is_received,
                received_at: None,
                reply,
                payment,
            })
        })
        .collect();

    infer_payment_receipt_state(&mut messages);
    messages
}

#[cfg(test)]
mod tests {
    use super::{
        decode_message_content, extract_appmsg_type, extract_payment_info,
        infer_payment_receipt_state, parse_amount_cents,
    };
    use crate::ia::types::{Message, PaymentInfo};

    const SAMPLE_TRANSFER_CONTENT_HEX: &str = "28B52FFD60AA032D1300569E6C40308BB60D10F0ADC733222241E6EB85D10C0BE14199FF24C6D51E58CC0FFCF6DF2B8B4B2BD112AD93889422303370E29ECA15F8F6D844727CD6107F188019540163005E0057000C444038428E47A4D3F1E9703C1C40F07139F18F8D0F0847A87F8BF3B2B0E8B810F2E0107210F26142874F0545010A02032A00CCEBBD5DF78C22E99A262B3F0663D39269B2B68A319574DA624C67245D9DB653BA2E934FE464DDAFEAFC188967359719DF516CEDCB6FEA61FB7BA39FAD8DB766016AA745FB69938C8CC9AD9AFA99C9CE1FC6A08E5180C67B5F7819DCF7F7FDB438DD9DBA1E6F7694DCAF6E8D7A78C28D04A08F0621C7135AD00845D000D258F0792021CCD00161A01052B061A44294AC24A809295D467885728AAA480B55A1DC5D7A45D92AD5F4A579430819D1F46FFFDC8CA2488A5366A0DE503CA30DC55AE6E36EDD9AF73298F9BCCC8217DB310BBB3B8390EED6D6FC86F88DE60E831886DDCFDA4F8B01D2D5691BA774C51929AA93D640C5D87856A9635BD564E8CDAF412C3FA6BEF6F9B36FB10DC59B6E8FA95B7B8E7B4BC5F373B7769F51A4AC262428168BB4756FD5A3985B65045559A4AC2895CAA5BB533C3F57054CA8D18D71024244C108A5052529742042832CAC6D12C0334290641CAD40918BF5FF3B8024D50D71942B18C2492922F281454762F5198422D1D7985C9C6B409737D83991DED48E889E058C9B771DDD3742A46817D3BED941FA2231E79F4189F02BC44CA3B3D5FAD61C801DD1C383081CD650F0D6B209E1CCE3A87D3F9A84F29247323EAEC691F8EDB1533CFFD7C615FD77C43046FD304A407E41EEC2485EC18603036ED8B8131955E8C59BC772E31BAA";
    const SAMPLE_TRANSFER_SOURCE_HEX: &str = "28B52FFD20853D030054053C6D7367736F757263653E0A093C7369676E61747572653E4E305F56315F63654F77784A51507C76315F652F564F37726D523C2F0A093C746D705F6E6F64093C7075626C69736865722D69643E3C2F0A093C2F3C2F05007C3018AA8D94B925E080286D0A0A";
    const SAMPLE_RED_PACKET_CONTENT_HEX: &str = "28B52FFD607D073D1A0086AA9042308D9B030005FB7E303030AC050F6E161F7AD558378EF945CD81FDC9646B6BA334648BB291DD3B4929F703DB74FEECF213DA35444476F7CA957E6450ABA5087608038D0073007A00DF89590CF907FA7C3C2EAC784FFEFB60E7DB0BC1FF3896037D24CB85FCD35AD2DECD84FCDB2F66BECF411F49A2083F51E43AB023651B0C8C898C0D149308A3C8C8C84387148E93110F38C69092910612510312514689C8A363ACF04134C164E4A181726181C02200808B896C133ECE2A15CBE672D180EF59804389CB4583F3F7B9C416EAEFF3AC62D96CA1CE024E6B555DAD5E6DBE35B1D7FE6A9AC557C5F09CA8E9977E55B1CBFAD85BD55EF8319D64B5ACBDAC8B5AD6C2D7C4BCD2AE89DF6AD485B1B4D3538FB16B52EC2DA5E4B4AC3B29B79ABDE51073612A5A6B65510CBBACE9925325AF29E7242B46492925DE1F62260DCEDDEFEDDCFD42E2166ACCA40917468E93112123291E1D23C8C7B8C1010AB8BD9BB34AC5E2754A29A96BC9A714E3EBAF461D4F14B6AEC9A7539C5FF4EED32F66154FDB39E4DFFD6CFBCB584E92B24E92E776DB1EB7E92C6E19AB3B9C8510220E20C649CAF9F1BA4ADDA7B5528C6A95D2FC52F3AA5C93D4A5EEF84EA7DCBA584E149B785A6CEF0C96CD166A29B3680FD4A44CCA2489FDD83DD05AB55258B5289D5E6711E7CE3D3BEFCBE170EEBCDEE6759D5FAFDB421DF24F82BF77C6794A39697467733A173D11A3E04001020338090418B0D0BCF60D7ED8A900BFC3DBB9DF3C0C5F33C8F6CE2B4914EFC9A2EF8F8E312344C406C808030511F4E2EFBF787F984103828451246CA8210E322515C88814252948E1204262A09B320F1200F3590EA350E59C2D2C3FCA9F643A28008E1840D5FAB0F3415A48833C13B70E22F0F5404C677EC05965E9F8A45B3B2F003607E66CCC8EBA23219AB09695EC21693827CE4F8C24CE25927A316A4F54D67FA5C71639D881CB9632400F35E7D449224DC7783DC40DFCC4AC05E5C2D0594E9F221D31FA2CC6C446F2E798D61CA56530FD90F0D0E1405FA0C069268D382FBB208D9176EC7B909963D34D2B6D001319745587174D2369A9385E00DB528D5346FAD014AC9081785BDC2DF36B373B647652629E0C4065B0EAA9ACB2958A36D8420176F7A9224A3442CA345029EF7FB2778AB7DEF4523104DC23F01155";
    const SAMPLE_RED_PACKET_SOURCE_HEX: &str = "28B52FFD20A7ED030092C6181D606B1E004A93F8DB688EF55DC1539172AF7BDA188AC04EC10104BC730EDCDC2871AFE7800A9BF747E3D1D91C691B256E02331404921D9A82441484325030EC9A5AB44062C17A2D6346FF49DD3A167F79187F74E1E4818EE6AFD3282DE8EAA51F5F7608001E6330541B21735F62041207E4A4F683736A08A618";

    #[test]
    fn parses_transfer_payment_metadata() {
        let content = decode_message_content(SAMPLE_TRANSFER_CONTENT_HEX, true);
        let source = decode_message_content(SAMPLE_TRANSFER_SOURCE_HEX, true);
        let payment = extract_payment_info(&content, 49).expect("transfer payment info");

        assert!(source.contains("<msgsource>"));
        assert_eq!(extract_appmsg_type(&content), Some(2000));
        assert_eq!(payment.kind, "transfer");
        assert_eq!(payment.amount_text.as_deref(), Some("￥0.10"));
        assert_eq!(payment.amount_cents, Some(10));
        assert_eq!(
            payment.transaction_id.as_deref(),
            Some("53010002627162202604101192831230")
        );
        assert_eq!(
            payment.transfer_id.as_deref(),
            Some("1000050001202604101036531173241")
        );
    }

    #[test]
    fn parses_red_packet_metadata() {
        let content = decode_message_content(SAMPLE_RED_PACKET_CONTENT_HEX, true);
        let source = decode_message_content(SAMPLE_RED_PACKET_SOURCE_HEX, true);
        let payment = extract_payment_info(&content, 49).expect("red-packet info");

        assert!(source.contains("<msgsource>"));
        assert_eq!(extract_appmsg_type(&content), Some(2001));
        assert_eq!(payment.kind, "red_packet");
        assert_eq!(payment.amount_text, None);
        assert_eq!(payment.amount_cents, None);
        assert_eq!(
            payment.send_id.as_deref(),
            Some("1000039801202604106156997548874")
        );
        assert_eq!(
            payment.pay_msg_id.as_deref(),
            Some("1000039801202604106156997548874")
        );
        assert_eq!(
            payment.receiver_title.as_deref(),
            Some("恭喜发财，大吉大利")
        );
    }

    #[test]
    fn parses_amount_text_to_cents() {
        assert_eq!(parse_amount_cents("￥0.10"), Some(10));
        assert_eq!(parse_amount_cents("¥12"), Some(1200));
        assert_eq!(parse_amount_cents("12.3元"), Some(1230));
    }

    fn transfer_message(local_id: i64, transaction_id: &str, is_self: bool) -> Message {
        Message {
            local_id,
            server_id: 0,
            chat_id: "APEX_GLORY".to_string(),
            sender: Some(if is_self {
                "wxid_self".to_string()
            } else {
                "APEX_GLORY".to_string()
            }),
            sender_name: None,
            msg_type: 49,
            kind: "transfer".to_string(),
            app_msg_type: Some(2000),
            content: "微信转账 ￥0.10".to_string(),
            timestamp: "2026-04-10T09:29:30+08:00".to_string(),
            is_mentioned: None,
            is_self: Some(is_self),
            is_received: Some(false),
            received_at: None,
            reply: None,
            payment: Some(PaymentInfo {
                kind: "transfer".to_string(),
                app_msg_type: Some(2000),
                amount_text: Some("￥0.10".to_string()),
                amount_cents: Some(10),
                currency: Some("CNY".to_string()),
                transaction_id: Some(transaction_id.to_string()),
                transfer_id: None,
                send_id: None,
                pay_msg_id: None,
                receiver_title: None,
                native_url: None,
            }),
        }
    }

    #[test]
    fn infers_received_transfer_from_matching_self_receipt() {
        let mut messages = vec![
            transfer_message(5, "new-transfer", false),
            transfer_message(4, "accepted-transfer", true),
            transfer_message(2, "accepted-transfer", false),
            transfer_message(1, "outgoing-unmatched", true),
        ];

        infer_payment_receipt_state(&mut messages);

        assert_eq!(messages[0].is_received, Some(false));
        assert_eq!(messages[1].is_received, Some(true));
        assert_eq!(messages[2].is_received, Some(true));
        assert_eq!(messages[3].is_received, Some(false));
    }
}
