use super::*;

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn outer(record: &str) -> String {
    format!("<msg><appmsg><title>Fixture</title><type>19</type><recorditem>{}</recorditem></appmsg></msg>", escape(record))
}

fn record(items: &str) -> String {
    format!("<recordinfo><datalist>{items}</datalist></recordinfo>")
}

fn item(sender: &str, body: &str) -> String {
    format!("<dataitem datatype='1'><datadesc>{}</datadesc><dataitemsource><sourcename>{}</sourcename></dataitemsource></dataitem>", escape(body), escape(sender))
}

fn nested(inner: &str, encoded: bool) -> String {
    let value = if encoded {
        escape(inner)
    } else {
        inner.to_owned()
    };
    format!("<dataitem datatype='17'><datatitle>Nested history</datatitle><sourcename>Forwarder</sourcename><recordxml>{value}</recordxml><dataitemsource><sourcename>Forwarder</sourcename></dataitemsource></dataitem>")
}

#[test]
fn observed_plain_bundle_preserves_text_and_media_descriptions() {
    let xml = include_str!("../tests/fixtures/merged-forward.xml");
    assert_eq!(render(xml).unwrap(), "[Chat History] Group conversation\nAlice: First forwarded text\nBob: [Photo]\nAlice: Last forwarded text\nCarol: [Sticker]");
}

#[test]
fn observed_nested_bundle_preserves_first_inner_message_without_phantom_media() {
    let xml = include_str!("../tests/fixtures/merged-forward-nested.xml");
    assert_eq!(render(xml).unwrap(), "[Chat History] Test conversation\nTest sender: Extra text\nTest sender: [Chat History] Group conversation\n  Alice: First forwarded text\n  Bob: [Photo]\n  Alice: Last forwarded text\n  Carol: [Sticker]");
}

#[test]
fn ordinary_unicode_messages_preserve_order() {
    assert_eq!(
        render(&outer(&record(
            &(item("Alice", "Hello") + &item("测试者", "中文🙂正文"))
        )))
        .unwrap(),
        "[Chat History] Fixture\nAlice: Hello\n测试者: 中文🙂正文"
    );
}

#[test]
fn entities_are_decoded_once_per_xml_layer_not_repeatedly() {
    let body = "Fish & Chips <three> \"quoted\" 'ok'; literal &lt; and &#65;";
    assert_eq!(
        render(&outer(&record(&item("Alice & Bob", body)))).unwrap(),
        format!("[Chat History] Fixture\nAlice & Bob: {body}")
    );
}

#[test]
fn numeric_entities_are_decoded() {
    let items = "<dataitem datatype='1'><datadesc>&#x4F60;&#x597D; &#38; &#39;ok&#39;</datadesc><sourcename>Alice</sourcename></dataitem>";
    assert_eq!(
        render(&outer(&record(items))).unwrap(),
        "[Chat History] Fixture\nAlice: 你好 & 'ok'"
    );
}

#[test]
fn cdata_xml_lookalikes_remain_message_text_not_metadata() {
    let body = "Write </dataitem> literally; <sourcename>Wrong</sourcename> <dataitem>not an item</dataitem>";
    let items = format!("<dataitem datatype='1'><datadesc><![CDATA[{body}]]></datadesc><sourcename>Alice</sourcename></dataitem>");
    assert_eq!(
        render(&outer(&record(&items))).unwrap(),
        format!("[Chat History] Fixture\nAlice: {body}")
    );
}

#[test]
fn cdata_record_and_adjacent_text_nodes_are_supported() {
    let items = "<dataitem datatype='1'><datadesc>before <![CDATA[<tag>]]> after</datadesc><displayname>Alice</displayname></dataitem>";
    // Split the outer CDATA before the inner record's CDATA terminator.
    let encoded_record = record(items).replace("]]>", "]]]]><![CDATA[>");
    let xml = format!("<msg><appmsg><title>Fixture</title><type>19</type><recorditem><![CDATA[{encoded_record}]]></recorditem></appmsg></msg>");
    assert_eq!(
        render(&xml).unwrap(),
        "[Chat History] Fixture\nAlice: before <tag> after"
    );
}

#[test]
fn nested_structural_and_escaped_records_preserve_following_sibling() {
    for encoded in [false, true] {
        let inside = record(&(item("Bob", "inner one") + &item("Charlie", "inner two")));
        let xml = outer(&record(
            &(nested(&inside, encoded) + &item("Dave", "after")),
        ));
        assert_eq!(render(&xml).unwrap(), "[Chat History] Fixture\nForwarder: [Chat History] Nested history\n  Bob: inner one\n  Charlie: inner two\nDave: after");
    }
}

#[test]
fn item_metadata_is_scoped_and_media_remains_display_only() {
    let items = "<dataitem datatype='2'><datatitle/><datadesc>[Photo]</datadesc><sourcename>Outer</sourcename><cdnurl>https://example.invalid/private</cdnurl><aeskey>private-key</aeskey></dataitem><dataitem datatype='34'><displayname>Voice sender</displayname></dataitem><dataitem datatype='8'><datatitle>report.pdf</datatitle><datadesc>description</datadesc></dataitem><dataitem datatype='37'><dataitemsource><displayname>Sticker sender</displayname></dataitemsource></dataitem>";
    assert_eq!(render(&outer(&record(items))).unwrap(), "[Chat History] Fixture\nOuter: [Photo]\nVoice sender: [media]\nreport.pdf\nSticker sender: [media]");
}

#[test]
fn sender_is_not_borrowed_from_nested_record() {
    let inner = record(&item("Inner sender", "inner"));
    let items = format!("<dataitem datatype='17'><datatitle>Nested</datatitle><recordxml>{inner}</recordxml></dataitem>");
    assert_eq!(
        render(&outer(&record(&items))).unwrap(),
        "[Chat History] Fixture\n[Chat History] Nested\n  Inner sender: inner"
    );
}

#[test]
fn invalid_or_missing_records_use_title_only_without_xml() {
    for record_xml in ["", "not XML", "<recordinfo>", "<recordinfo><datalist/></recordinfo>",
        "<recordinfo><wrapper><datalist><dataitem><datadesc>hidden</datadesc></dataitem></datalist></wrapper></recordinfo>"] {
        assert_eq!(render(&outer(record_xml)).unwrap(), "Fixture");
    }
    assert_eq!(
        render("<msg><appmsg><title>Fixture</title><type>19</type></appmsg></msg>").unwrap(),
        "Fixture"
    );
    assert_eq!(
        render("<msg><appmsg><type>19</type></appmsg></msg>").unwrap(),
        "[Chat History unavailable]"
    );
}

#[test]
fn invalid_nested_record_is_labeled_without_losing_later_messages() {
    for extra in ["", "<recordxml>bad XML</recordxml>"] {
        let items = format!(
            "<dataitem datatype='17'><datatitle>Nested</datatitle>{extra}</dataitem>{}",
            item("Alice", "after")
        );
        assert_eq!(render(&outer(&record(&items))).unwrap(), "[Chat History] Fixture\n[Chat History] Nested\n  [Nested chat history unavailable]\nAlice: after");
    }
}

#[test]
fn unrelated_appmsg_types_are_not_interpreted_as_chat_history() {
    for kind in [3, 4, 5, 6, 57] {
        assert!(render(&format!("<msg><appmsg><title>Title</title><type>{kind}</type><recorditem>{}</recorditem></appmsg></msg>", escape(&record(&item("Alice", "text"))))).is_none());
    }
    assert!(render(
        "<msg><appmsg><title>Title</title><refermsg><type>19</type></refermsg></appmsg></msg>"
    )
    .is_none());
}

#[test]
fn outer_attributes_and_literal_type_tags_in_title_do_not_change_subtype() {
    let xml = format!("<msg version='1'><appmsg><title><![CDATA[<type>5</type>]]></title><type>19</type><recorditem>{}</recorditem></appmsg></msg>", escape(&record(&item("Alice", "text"))));
    assert_eq!(
        render(&xml).unwrap(),
        "[Chat History] <type>5</type>\nAlice: text"
    );
}

#[test]
fn dtd_and_excessive_xml_nodes_are_rejected_in_each_layer() {
    let dtd = "<!DOCTYPE recordinfo [<!ENTITY private 'private-data'>]><recordinfo><datalist><dataitem><datadesc>&private;</datadesc></dataitem></datalist></recordinfo>";
    assert_eq!(render(&outer(dtd)).unwrap(), "Fixture");
    assert!(render(
        "<!DOCTYPE msg [<!ENTITY private '19'>]><msg><appmsg><type>&private;</type></appmsg></msg>"
    )
    .is_none());
    let many_nodes = format!(
        "<recordinfo><datalist>{}</datalist></recordinfo>",
        "<node/>".repeat(MAX_XML_NODES as usize)
    );
    assert_eq!(render(&outer(&many_nodes)).unwrap(), "Fixture");
    let xml = format!(
        "<msg>{}<appmsg><type>19</type></appmsg></msg>",
        "<node/>".repeat(MAX_XML_NODES as usize)
    );
    assert!(render(&xml).is_none());
}

#[test]
fn namespaces_and_unrelated_dataitem_descendants_are_not_metadata() {
    let xml = "<msg xmlns='other'><appmsg><type>19</type></appmsg></msg>";
    assert!(render(xml).is_none());
    let items = "<wrapper><dataitem><datadesc>hidden</datadesc></dataitem></wrapper><dataitemsource><datadesc>not a message</datadesc></dataitemsource>";
    assert_eq!(render(&outer(&record(items))).unwrap(), "Fixture");
}

#[test]
fn large_normal_bundle_preserves_all_150_messages() {
    let items: String = (0..150)
        .map(|i| item("Alice", &format!("message {i}")))
        .collect();
    let expected = "[Chat History] Fixture\n".to_owned()
        + &(0..150)
            .map(|i| format!("Alice: message {i}"))
            .collect::<Vec<_>>()
            .join("\n");
    assert_eq!(render(&outer(&record(&items))).unwrap(), expected);
}

#[test]
fn item_limit_is_global_across_nested_records_and_visible() {
    let inner: String = (0..MAX_ITEMS)
        .map(|i| item("Alice", &format!("message {i}")))
        .collect();
    let xml = outer(&record(&nested(&record(&inner), false)));
    let output = render(&xml).unwrap();
    assert!(output.contains(&format!("message {}", MAX_ITEMS - 2)));
    assert!(!output.contains(&format!("message {}", MAX_ITEMS - 1)));
    assert!(output.ends_with(TRUNCATED));
    assert_eq!(output.matches("Alice:").count(), MAX_ITEMS - 1);
}

#[test]
fn depth_limit_is_visible_and_preserves_outer_siblings() {
    let mut inside = record(&item("Alice", "too deep"));
    for _ in 0..MAX_DEPTH + 2 {
        inside = record(&nested(&inside, false));
    }
    let xml = outer(&record(&(nested(&inside, false) + &item("Bob", "after"))));
    let output = render(&xml).unwrap();
    assert!(output.contains("[Nested chat history omitted: depth limit]"));
    assert!(!output.contains("too deep"));
    assert!(output.ends_with("Bob: after"));
}

#[test]
fn utf8_output_and_fallback_title_limits_have_explicit_markers() {
    let body = "中文🙂".repeat(15_000);
    let output = render(&outer(&record(&item("Alice", &body)))).unwrap();
    assert!(output.len() <= MAX_OUTPUT_BYTES);
    assert!(output.ends_with(TRUNCATED));
    assert!(std::str::from_utf8(output.as_bytes()).is_ok());
    let title = fallback_title(&body);
    assert!(title.len() <= MAX_OUTPUT_BYTES);
    assert!(title.ends_with(TRUNCATED));
}

#[test]
fn total_xml_parse_budget_is_bounded() {
    let xml = format!(
        "<recorditem>{}</recorditem>",
        escape(&record(&item("Alice", "text")))
    );
    let document = parse(&xml).unwrap();
    let mut renderer = Renderer {
        parsed_bytes: MAX_PARSED_BYTES,
        ..Default::default()
    };
    assert!(renderer.embedded_record(document.root_element(), 0));
    assert!(renderer.truncated);
    assert!(renderer.output.ends_with(TRUNCATED));
    assert!(parse(&" ".repeat(MAX_XML_BYTES + 1)).is_none());
}
