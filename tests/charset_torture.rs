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
