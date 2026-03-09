//! XML pull parser for Discogs label data dumps.
//!
//! Uses `quick-xml` to parse `<label>` elements from Discogs labels XML dumps,
//! yielding `Label` structs. Only extracts id, name, and parentLabel.
//! Supports both plain XML and gzipped input.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use log::info;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::label_model::*;

/// Parse labels from an XML file (plain or gzipped).
///
/// Detects gzip by `.gz` extension. Yields labels via the callback.
pub fn parse_labels<F>(path: &Path, mut callback: F) -> Result<usize>
where
    F: FnMut(Label),
{
    let is_gzip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));

    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    if is_gzip {
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        parse_labels_from_reader(reader, &mut callback)
    } else {
        let reader = BufReader::new(file);
        parse_labels_from_reader(reader, &mut callback)
    }
}

/// Parse labels from any BufRead source.
fn parse_labels_from_reader<R, F>(reader: R, callback: &mut F) -> Result<usize>
where
    R: BufRead,
    F: FnMut(Label),
{
    let mut xml_reader = Reader::from_reader(reader);
    xml_reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut inner_buf = Vec::new();
    let mut count: usize = 0;

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"label" => {
                let label = parse_label_body(&mut xml_reader, &mut inner_buf)?;
                callback(label);
                count += 1;

                if count.is_multiple_of(100_000) {
                    info!("Parsed {} labels...", count);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "XML parse error at position {}: {}",
                    xml_reader.error_position(),
                    e
                ))
            }
            _ => {}
        }
        buf.clear();
    }

    info!("Parsed {} labels total", count);
    Ok(count)
}

/// Parse the body of a `<label>` element.
fn parse_label_body<R: BufRead>(reader: &mut Reader<R>, buf: &mut Vec<u8>) -> Result<Label> {
    let mut label = Label::default();
    let mut current_text = String::new();
    let mut in_sublabels = false;
    let mut in_parent_label = false;

    buf.clear();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(ref e)) => {
                let qname = e.name();
                let name = qname.as_ref();
                current_text.clear();

                match name {
                    b"sublabels" => in_sublabels = true,
                    b"parentLabel" => {
                        in_parent_label = true;
                        for attr in e.attributes() {
                            let attr = attr?;
                            if attr.key.as_ref() == b"id" {
                                let val = attr.unescape_value()?;
                                label.parent_id = Some(val.parse().unwrap_or(0));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                current_text.push_str(&e.unescape()?);
            }
            Ok(Event::CData(ref e)) => {
                current_text.push_str(&String::from_utf8_lossy(e.as_ref()));
            }
            Ok(Event::End(ref e)) => {
                let qname = e.name();
                let name = qname.as_ref();

                match name {
                    b"label" if !in_sublabels => {
                        buf.clear();
                        return Ok(label);
                    }
                    b"label" if in_sublabels => {
                        // closing a <label> inside <sublabels> — ignore
                    }
                    b"sublabels" => in_sublabels = false,
                    b"id" => {
                        if !in_sublabels {
                            label.id = current_text.trim().parse().unwrap_or(0);
                        }
                    }
                    b"name" => {
                        if !in_sublabels && !in_parent_label {
                            label.name = current_text.trim().to_string();
                        }
                    }
                    b"parentLabel" => {
                        label.parent_name = current_text.trim().to_string();
                        in_parent_label = false;
                    }
                    _ => {}
                }

                current_text.clear();
            }
            Ok(Event::Empty(ref _e)) => {
                // Skip empty elements
            }
            Ok(Event::Eof) => {
                return Err(anyhow::anyhow!("Unexpected EOF inside <label>"));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("XML parse error: {}", e));
            }
            _ => {}
        }
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn test_parse_labels() {
        let path = fixture_path("labels_fixture.xml");
        let mut labels = Vec::new();
        let count = parse_labels(&path, |l| labels.push(l)).unwrap();

        assert_eq!(count, 8);
        assert_eq!(labels.len(), 8);
    }

    #[test]
    fn test_label_without_parent() {
        let path = fixture_path("labels_fixture.xml");
        let mut labels = Vec::new();
        parse_labels(&path, |l| labels.push(l)).unwrap();

        let emi = &labels[0];
        assert_eq!(emi.id, 1);
        assert_eq!(emi.name, "EMI");
        assert_eq!(emi.parent_id, None);
        assert_eq!(emi.parent_name, "");
    }

    #[test]
    fn test_label_with_parent() {
        let path = fixture_path("labels_fixture.xml");
        let mut labels = Vec::new();
        parse_labels(&path, |l| labels.push(l)).unwrap();

        let parlophone = &labels[1];
        assert_eq!(parlophone.id, 2);
        assert_eq!(parlophone.name, "Parlophone");
        assert_eq!(parlophone.parent_id, Some(1));
        assert_eq!(parlophone.parent_name, "EMI");

        let capitol = &labels[2];
        assert_eq!(capitol.id, 3);
        assert_eq!(capitol.name, "Capitol Records");
        assert_eq!(capitol.parent_id, Some(1));
        assert_eq!(capitol.parent_name, "EMI");
    }

    #[test]
    fn test_label_standalone() {
        let path = fixture_path("labels_fixture.xml");
        let mut labels = Vec::new();
        parse_labels(&path, |l| labels.push(l)).unwrap();

        let sub_pop = &labels[3];
        assert_eq!(sub_pop.id, 4);
        assert_eq!(sub_pop.name, "Sub Pop");
        assert_eq!(sub_pop.parent_id, None);

        let drag_city = &labels[4];
        assert_eq!(drag_city.id, 5);
        assert_eq!(drag_city.name, "Drag City");
        assert_eq!(drag_city.parent_id, None);
    }

    #[test]
    fn test_labels_with_parent_chain() {
        let path = fixture_path("labels_fixture.xml");
        let mut labels = Vec::new();
        parse_labels(&path, |l| labels.push(l)).unwrap();

        // Matador Records -> Beggars Group
        let matador = &labels[5];
        assert_eq!(matador.id, 6);
        assert_eq!(matador.name, "Matador Records");
        assert_eq!(matador.parent_id, Some(7));
        assert_eq!(matador.parent_name, "Beggars Group");

        // 4AD -> Beggars Group
        let four_ad = &labels[7];
        assert_eq!(four_ad.id, 8);
        assert_eq!(four_ad.name, "4AD");
        assert_eq!(four_ad.parent_id, Some(7));
        assert_eq!(four_ad.parent_name, "Beggars Group");
    }
}
