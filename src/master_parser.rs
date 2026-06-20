//! XML pull parser for Discogs master release data dumps.
//!
//! Uses `quick-xml` to parse `<master>` elements from Discogs masters XML dumps,
//! yielding `Master` structs. Extracts id (from attribute), title, main_release,
//! year, and artists. Genres and styles are skipped.
//! Supports both plain XML and gzipped input.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use log::info;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::master_model::*;
use crate::parser::{resolve_general_ref, unescape_attr, unescape_text};

/// Parse masters from an XML file (plain or gzipped).
///
/// Detects gzip by `.gz` extension. Yields masters via the callback.
pub fn parse_masters<F>(path: &Path, mut callback: F) -> Result<usize>
where
    F: FnMut(Master),
{
    let is_gzip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));

    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    if is_gzip {
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        parse_masters_from_reader(reader, &mut callback)
    } else {
        let reader = BufReader::new(file);
        parse_masters_from_reader(reader, &mut callback)
    }
}

/// Parse masters from any BufRead source.
fn parse_masters_from_reader<R, F>(reader: R, callback: &mut F) -> Result<usize>
where
    R: BufRead,
    F: FnMut(Master),
{
    let mut xml_reader = Reader::from_reader(reader);
    xml_reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut inner_buf = Vec::new();
    let mut count: usize = 0;

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"master" => {
                // Master id is an attribute: <master id="113">
                let mut master_id: u64 = 0;
                for attr in e.attributes() {
                    let attr = attr?;
                    if attr.key.as_ref() == b"id" {
                        let val = unescape_attr(&attr)?;
                        master_id = val.parse().unwrap_or(0);
                    }
                }

                let master = parse_master_body(&mut xml_reader, &mut inner_buf, master_id)?;
                callback(master);
                count += 1;

                if count.is_multiple_of(100_000) {
                    info!("Parsed {} masters...", count);
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

    info!("Parsed {} masters total", count);
    Ok(count)
}

/// Parse the body of a `<master>` element.
fn parse_master_body<R: BufRead>(
    reader: &mut Reader<R>,
    buf: &mut Vec<u8>,
    master_id: u64,
) -> Result<Master> {
    let mut master = Master {
        id: master_id,
        ..Default::default()
    };

    let mut current_text = String::new();
    let mut in_artists = false;
    let mut in_artist = false;
    let mut in_genres = false;
    let mut in_styles = false;
    let mut in_videos = false;
    let mut current_artist = MasterArtist::default();

    buf.clear();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                current_text.clear();

                match name.as_ref() {
                    b"artists" => in_artists = true,
                    b"artist" if in_artists => {
                        in_artist = true;
                        current_artist = MasterArtist::default();
                    }
                    b"genres" => in_genres = true,
                    b"styles" => in_styles = true,
                    b"videos" => in_videos = true,
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                current_text.push_str(&unescape_text(e)?);
            }
            Ok(Event::GeneralRef(ref e)) => {
                current_text.push_str(&resolve_general_ref(e)?);
            }
            Ok(Event::CData(ref e)) => {
                current_text.push_str(&String::from_utf8_lossy(e.as_ref()));
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();

                match name.as_ref() {
                    b"master" => {
                        buf.clear();
                        return Ok(master);
                    }
                    b"artists" => in_artists = false,
                    b"artist" if in_artists => {
                        master.artists.push(current_artist.clone());
                        in_artist = false;
                    }
                    b"genres" => in_genres = false,
                    b"styles" => in_styles = false,
                    b"videos" => in_videos = false,
                    b"id" if in_artist => {
                        current_artist.id = current_text.trim().parse().unwrap_or(0);
                    }
                    b"name" if in_artist => {
                        current_artist.name = current_text.trim().to_string();
                    }
                    b"title" if !in_genres && !in_styles && !in_videos => {
                        master.title = current_text.trim().to_string();
                    }
                    b"main_release" => {
                        let val: u64 = current_text.trim().parse().unwrap_or(0);
                        if val > 0 {
                            master.main_release_id = Some(val);
                        }
                    }
                    b"year" if !in_genres && !in_styles && !in_videos => {
                        let val: u16 = current_text.trim().parse().unwrap_or(0);
                        if val > 0 {
                            master.year = Some(val);
                        }
                    }
                    _ => {}
                }

                current_text.clear();
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Eof) => {
                return Err(anyhow::anyhow!("Unexpected EOF inside <master>"));
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

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("masters_fixture.xml")
    }

    #[test]
    fn test_parse_masters() {
        let mut masters = Vec::new();
        let count = parse_masters(&fixture_path(), |m| masters.push(m)).unwrap();

        assert_eq!(count, 5);
        assert_eq!(masters.len(), 5);
    }

    #[test]
    fn test_master_single_artist() {
        let mut masters = Vec::new();
        parse_masters(&fixture_path(), |m| masters.push(m)).unwrap();

        let stereolab = &masters[0];
        assert_eq!(stereolab.id, 100);
        assert_eq!(stereolab.title, "Aluminum Tunes");
        assert_eq!(stereolab.year, Some(1998));
        assert_eq!(stereolab.main_release_id, Some(1001));
        assert_eq!(stereolab.artists.len(), 1);
        assert_eq!(stereolab.artists[0].id, 3000);
        assert_eq!(stereolab.artists[0].name, "Stereolab");
    }

    #[test]
    fn test_master_multiple_artists() {
        let mut masters = Vec::new();
        parse_masters(&fixture_path(), |m| masters.push(m)).unwrap();

        let duke = &masters[2];
        assert_eq!(duke.id, 300);
        assert_eq!(duke.artists.len(), 2);
        assert_eq!(duke.artists[0].name, "Duke Ellington");
        assert_eq!(duke.artists[1].name, "John Coltrane");
        assert_eq!(duke.title, "Duke Ellington & John Coltrane");
    }

    #[test]
    fn test_master_without_year() {
        let mut masters = Vec::new();
        parse_masters(&fixture_path(), |m| masters.push(m)).unwrap();

        let sessa = &masters[4];
        assert_eq!(sessa.id, 500);
        assert_eq!(sessa.year, None);
        assert_eq!(sessa.title, "Pequena Vertigem de Amor");
    }

    #[test]
    fn test_master_without_main_release() {
        let mut masters = Vec::new();
        parse_masters(&fixture_path(), |m| masters.push(m)).unwrap();

        let sessa = &masters[4];
        assert_eq!(sessa.main_release_id, None);
    }

    #[test]
    fn test_master_without_genres_styles() {
        let mut masters = Vec::new();
        parse_masters(&fixture_path(), |m| masters.push(m)).unwrap();

        // Master 400 has no genres/styles elements
        let jessica = &masters[3];
        assert_eq!(jessica.id, 400);
        assert_eq!(jessica.title, "On Your Own Love Again");
        assert_eq!(jessica.artists[0].name, "Jessica Pratt");
    }
}
