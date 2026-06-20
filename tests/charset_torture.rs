//! WX-1.2.8 detector for the XML stream filter.
//!
//! Drives the cross-repo `@wxyc/shared` charset-torture corpus through
//! `parse_release_from_bytes` by synthesizing a minimal `<release>` XML
//! fragment per entry and asserting the parsed `Release.title` is byte-equal
//! to the input. Catches future regressions in the `quick-xml` SAX path or
//! any custom decoder added to the streaming filter.
//!
//! See WXYC/docs#15 for the WX-1 plan.

use std::collections::HashMap;
use std::path::Path;

use discogs_xml_converter::parser::parse_release_from_bytes;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct CorpusEntry {
    input: String,
    notes: String,
}

#[derive(Deserialize)]
struct Corpus {
    categories: HashMap<String, Vec<CorpusEntry>>,
}

fn load_corpus() -> Corpus {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/charset-torture.json");
    let bytes = std::fs::read(&path).expect("vendored corpus exists");
    serde_json::from_slice(&bytes).expect("corpus is valid JSON")
}

/// Inputs whose `parse_release_from_bytes` round-trip cannot succeed today.
fn expected_failures() -> HashMap<&'static str, &'static str> {
    HashMap::new()
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn build_release_xml(title: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<release id="1" status="Accepted">
  <artists><artist><id>1</id><name>Test</name><anv></anv><join></join><role></role><tracks></tracks></artist></artists>
  <title>{}</title>
  <country></country>
  <released></released>
  <notes></notes>
  <data_quality></data_quality>
  <formats><format name="CD" qty="1"></format></formats>
  <labels><label name="L" catno="C"/></labels>
  <genres></genres>
  <styles></styles>
  <tracklist></tracklist>
  <identifiers></identifiers>
  <videos></videos>
  <companies></companies>
  <extraartists></extraartists>
</release>"#,
        xml_escape(title)
    )
}

#[test]
fn corpus_xml_title_roundtrip() {
    let corpus = load_corpus();
    let known_failures = expected_failures();

    let mut unexpected_failures: Vec<String> = Vec::new();
    let mut unexpected_passes: Vec<String> = Vec::new();

    for (category, entries) in &corpus.categories {
        for entry in entries {
            let xml = build_release_xml(&entry.input);
            let result = parse_release_from_bytes(xml.as_bytes());
            let known = known_failures.get(entry.input.as_str()).copied();

            let passed = match &result {
                Ok(release) => release.title == entry.input,
                Err(_) => false,
            };

            match (passed, known) {
                (true, None) => {}
                (true, Some(_tag)) => {
                    unexpected_passes.push(format!(
                        "{category}: {input:?} now round-trips; remove from EXPECTED_FAILURES",
                        input = entry.input
                    ));
                }
                (false, Some(_tag)) => {}
                (false, None) => match result {
                    Ok(release) => unexpected_failures.push(format!(
                        "{category}: {input:?} -> title={got:?}\n    notes: {notes}",
                        input = entry.input,
                        got = release.title,
                        notes = entry.notes,
                    )),
                    Err(e) => unexpected_failures.push(format!(
                        "{category}: {input:?} -> parse error: {e}\n    notes: {notes}",
                        input = entry.input,
                        notes = entry.notes,
                    )),
                },
            }
        }
    }

    let mut report = String::new();
    if !unexpected_failures.is_empty() {
        report.push_str(&format!(
            "\nUnexpected failures ({}):\n  {}\n",
            unexpected_failures.len(),
            unexpected_failures.join("\n  ")
        ));
    }
    if !unexpected_passes.is_empty() {
        report.push_str(&format!(
            "\nUnexpected passes ({}):\n  {}\n",
            unexpected_passes.len(),
            unexpected_passes.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");
}

/// Entity-unescaping parity guard for the quick-xml 0.40 migration.
///
/// The vendored charset-torture corpus exercises Unicode/encoding round-trips
/// but contains no XML entity references (`&amp;`, `&lt;`, ...), so it cannot
/// catch a decode/unescape regression. quick-xml 0.40 removed
/// `BytesText::unescape()` and changed `Attribute::unescape_value()` to apply
/// attribute-value whitespace normalization; getting the replacement wrong
/// would either leave `&amp;` unresolved, double-unescape (`&amp;lt;` -> `<`),
/// or silently collapse whitespace. This test pins the exact decoded output for
/// the five predefined entities and a numeric character reference, in both text
/// content (`<title>`, `<notes>`) and an attribute value (`<format name=...>`).
#[test]
fn entity_references_decode_exactly_once() {
    // Raw XML with pre-escaped entities placed verbatim (NOT via xml_escape):
    // - title: predefined entities `& < > " '`
    // - notes: a numeric character reference for `&` (`&#38;`) plus a predefined one
    // - format name attribute: `&amp;` and `&lt;`
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<release id="42" status="Accepted">
  <artists><artist><id>1</id><name>Test</name><anv></anv><join></join><role></role><tracks></tracks></artist></artists>
  <title>Sun &amp; Steel &lt;mix&gt; &quot;live&quot; o&apos;clock</title>
  <country></country>
  <released></released>
  <notes>R&#38;B &amp; soul</notes>
  <data_quality></data_quality>
  <formats><format name="7&quot; &amp; 12&quot; &lt;reissue&gt;" qty="1"></format></formats>
  <labels><label name="L" catno="C"/></labels>
  <genres></genres>
  <styles></styles>
  <tracklist></tracklist>
  <identifiers></identifiers>
  <videos></videos>
  <companies></companies>
  <extraartists></extraartists>
</release>"#;

    let release = parse_release_from_bytes(xml.as_bytes()).expect("parses");

    // Text content: each entity resolved to its single literal character, once.
    assert_eq!(release.title, r#"Sun & Steel <mix> "live" o'clock"#);
    // Numeric character reference + predefined entity both resolve.
    assert_eq!(release.notes, "R&B & soul");
    // Attribute value: predefined entities resolved, no whitespace normalization.
    assert_eq!(release.formats.len(), 1);
    assert_eq!(release.formats[0].name, r#"7" & 12" <reissue>"#);
}

/// Guards the non-normalizing attribute path: the old `unescape_value()` did
/// NOT collapse embedded tabs/newlines, but 0.40's `normalized_value()` (and its
/// deprecated `unescape_value()` shim) would. A tab/newline inside an attribute
/// value must survive verbatim.
#[test]
fn attribute_whitespace_is_not_normalized() {
    // Literal TAB and LF embedded directly in the format name attribute.
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<release id=\"7\" status=\"Accepted\">\n\
  <artists><artist><id>1</id><name>Test</name><anv></anv><join></join><role></role><tracks></tracks></artist></artists>\n\
  <title>t</title>\n\
  <country></country><released></released><notes></notes><data_quality></data_quality>\n\
  <formats><format name=\"a&#9;b&#10;c\" qty=\"1\"></format></formats>\n\
  <labels></labels><genres></genres><styles></styles><tracklist></tracklist>\n\
  <identifiers></identifiers><videos></videos><companies></companies><extraartists></extraartists>\n\
</release>";

    let release = parse_release_from_bytes(xml.as_bytes()).expect("parses");
    assert_eq!(release.formats.len(), 1);
    // TAB (U+0009) and LF (U+000A) preserved verbatim, NOT collapsed to spaces.
    assert_eq!(release.formats[0].name, "a\tb\nc");
}
