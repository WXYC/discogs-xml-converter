//! XML pull parser for Discogs artist data dumps.
//!
//! Uses `quick-xml` to parse `<artist>` elements from Discogs artists XML dumps,
//! yielding `Artist` structs. Supports both plain XML and gzipped input.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use log::info;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::artist_model::*;

/// Parse artists from an XML file (plain or gzipped).
///
/// Detects gzip by `.gz` extension. Yields artists via the callback.
pub fn parse_artists<F>(path: &Path, mut callback: F) -> Result<usize>
where
    F: FnMut(Artist),
{
    let is_gzip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));

    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    if is_gzip {
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        parse_artists_from_reader(reader, &mut callback)
    } else {
        let reader = BufReader::new(file);
        parse_artists_from_reader(reader, &mut callback)
    }
}

/// Parse artists from any BufRead source.
fn parse_artists_from_reader<R, F>(reader: R, callback: &mut F) -> Result<usize>
where
    R: BufRead,
    F: FnMut(Artist),
{
    let mut xml_reader = Reader::from_reader(reader);
    xml_reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut inner_buf = Vec::new();
    let mut count: usize = 0;

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"artist" => {
                let artist = parse_artist_body(&mut xml_reader, &mut inner_buf)?;
                callback(artist);
                count += 1;

                if count.is_multiple_of(100_000) {
                    info!("Parsed {} artists...", count);
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

    info!("Parsed {} artists total", count);
    Ok(count)
}

/// Parse the body of an `<artist>` element.
fn parse_artist_body<R: BufRead>(reader: &mut Reader<R>, buf: &mut Vec<u8>) -> Result<Artist> {
    let mut artist = Artist::default();
    let mut current_text = String::new();

    let mut in_namevariations = false;
    let mut in_aliases = false;
    let mut in_members = false;
    let mut current_member_id: u64 = 0;

    buf.clear();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(ref e)) => {
                let qname = e.name();
                let name = qname.as_ref();
                current_text.clear();

                match name {
                    b"namevariations" => in_namevariations = true,
                    b"aliases" => in_aliases = true,
                    b"members" => in_members = true,
                    b"name" => {
                        if in_aliases || in_members {
                            // Extract id attribute from <name id="...">
                            for attr in e.attributes() {
                                let attr = attr?;
                                if attr.key.as_ref() == b"id" {
                                    let val = attr.unescape_value()?;
                                    current_member_id = val.parse().unwrap_or(0);
                                }
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
                    b"artist" => {
                        buf.clear();
                        return Ok(artist);
                    }
                    b"id" => {
                        if !in_namevariations && !in_aliases && !in_members {
                            artist.id = current_text.trim().parse().unwrap_or(0);
                        }
                    }
                    b"name" => {
                        let text = current_text.trim().to_string();
                        if !text.is_empty() {
                            if in_namevariations {
                                artist.name_variations.push(text);
                            } else if in_aliases {
                                artist.aliases.push(text);
                            } else if in_members {
                                artist.members.push(Member {
                                    id: current_member_id,
                                    name: text,
                                });
                                current_member_id = 0;
                            } else {
                                artist.name = text;
                            }
                        }
                    }
                    b"namevariations" => in_namevariations = false,
                    b"aliases" => in_aliases = false,
                    b"members" => in_members = false,
                    _ => {}
                }

                current_text.clear();
            }
            Ok(Event::Eof) => {
                return Err(anyhow::anyhow!("Unexpected EOF inside <artist>"));
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
    fn test_parse_artists() {
        let path = fixture_path("artists_fixture.xml");
        let mut artists = Vec::new();
        let count = parse_artists(&path, |a| artists.push(a)).unwrap();

        assert_eq!(count, 5);
        assert_eq!(artists.len(), 5);
    }

    #[test]
    fn test_artist_with_aliases_and_namevariations() {
        let path = fixture_path("artists_fixture.xml");
        let mut artists = Vec::new();
        parse_artists(&path, |a| artists.push(a)).unwrap();

        let pdiddy = &artists[0];
        assert_eq!(pdiddy.id, 123);
        assert_eq!(pdiddy.name, "P. Diddy");
        assert_eq!(pdiddy.name_variations, vec!["P Diddy", "Puff Daddy"]);
        assert_eq!(pdiddy.aliases, vec!["Sean Combs", "Diddy"]);
        assert_eq!(pdiddy.members.len(), 1);
        assert_eq!(pdiddy.members[0].id, 1001);
        assert_eq!(pdiddy.members[0].name, "Member One");
    }

    #[test]
    fn test_artist_with_unicode() {
        let path = fixture_path("artists_fixture.xml");
        let mut artists = Vec::new();
        parse_artists(&path, |a| artists.push(a)).unwrap();

        let bjork = &artists[1];
        assert_eq!(bjork.id, 200);
        assert_eq!(bjork.name, "Björk");
        assert_eq!(bjork.name_variations, vec!["Bjork", "Björk Guðmundsdóttir"]);
        assert!(bjork.aliases.is_empty());
    }

    #[test]
    fn test_artist_with_members() {
        let path = fixture_path("artists_fixture.xml");
        let mut artists = Vec::new();
        parse_artists(&path, |a| artists.push(a)).unwrap();

        let radiohead = &artists[2];
        assert_eq!(radiohead.id, 300);
        assert_eq!(radiohead.name, "Radiohead");
        assert_eq!(radiohead.members.len(), 5);
        assert_eq!(radiohead.members[0].id, 301);
        assert_eq!(radiohead.members[0].name, "Thom Yorke");
    }

    #[test]
    fn test_artist_without_extras() {
        let path = fixture_path("artists_fixture.xml");
        let mut artists = Vec::new();
        parse_artists(&path, |a| artists.push(a)).unwrap();

        let minimal = &artists[4];
        assert_eq!(minimal.id, 500);
        assert_eq!(minimal.name, "Artist Without Extras");
        assert!(minimal.aliases.is_empty());
        assert!(minimal.name_variations.is_empty());
        assert!(minimal.members.is_empty());
    }
}
