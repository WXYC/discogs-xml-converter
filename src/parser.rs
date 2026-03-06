//! XML pull parser for Discogs release data dumps.
//!
//! Uses `quick-xml` to parse `<release>` elements from Discogs XML dumps,
//! yielding `Release` structs. Supports both plain XML and gzipped input.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use log::info;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::model::*;

/// Parse releases from an XML file (plain or gzipped).
///
/// Detects gzip by `.gz` extension. Yields releases via the callback.
/// Skips releases with no artists. Stops after `limit` releases if set.
pub fn parse_releases<F>(
    path: &Path,
    limit: Option<usize>,
    progress_interval: usize,
    mut callback: F,
) -> Result<usize>
where
    F: FnMut(Release),
{
    let is_gzip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));

    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    if is_gzip {
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        parse_releases_from_reader(reader, limit, progress_interval, &mut callback)
    } else {
        let reader = BufReader::new(file);
        parse_releases_from_reader(reader, limit, progress_interval, &mut callback)
    }
}

/// Parse releases from any BufRead source.
fn parse_releases_from_reader<R, F>(
    reader: R,
    limit: Option<usize>,
    progress_interval: usize,
    callback: &mut F,
) -> Result<usize>
where
    R: BufRead,
    F: FnMut(Release),
{
    let mut xml_reader = Reader::from_reader(reader);
    xml_reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut inner_buf = Vec::new();
    let mut count: usize = 0;
    let mut total_seen: usize = 0;

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"release" => {
                // Extract attributes before releasing the borrow on buf
                let mut id: u64 = 0;
                let mut status = String::new();
                for attr in e.attributes() {
                    let attr = attr?;
                    match attr.key.as_ref() {
                        b"id" => {
                            let val = attr.unescape_value()?;
                            id = val.parse().unwrap_or(0);
                        }
                        b"status" => {
                            status = attr.unescape_value()?.to_string();
                        }
                        _ => {}
                    }
                }

                let release = parse_release_body(&mut xml_reader, id, status, &mut inner_buf)?;
                total_seen += 1;

                // Skip releases with no artists
                if release.artists.is_empty() {
                    buf.clear();
                    continue;
                }

                callback(release);
                count += 1;

                if count.is_multiple_of(progress_interval) {
                    info!("Processed {} releases ({} seen)...", count, total_seen);
                }

                if let Some(lim) = limit {
                    if count >= lim {
                        info!("Reached limit of {} releases", lim);
                        break;
                    }
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

    info!(
        "Parsed {} releases ({} seen, {} skipped without artists)",
        count,
        total_seen,
        total_seen - count
    );
    Ok(count)
}

/// Parse the body of a `<release>` element (after the start tag attributes
/// have already been extracted).
fn parse_release_body<R: BufRead>(
    reader: &mut Reader<R>,
    id: u64,
    status: String,
    buf: &mut Vec<u8>,
) -> Result<Release> {
    let mut release = Release {
        id,
        status,
        ..Default::default()
    };

    // Track the current element path for context
    let mut path: Vec<String> = vec!["release".to_string()];

    // State for text collection
    let mut current_text = String::new();

    // Track artist parsing state
    let mut in_artists = false;
    let mut in_extraartists = false;
    let mut in_track_artists = false;
    let mut in_track_extraartists = false;
    let mut current_artist = ReleaseArtist::default();
    let mut current_track_artist = TrackArtist::default();
    let mut artist_position: u32 = 0;
    let mut current_track = ReleaseTrack::default();
    let mut in_tracklist = false;
    let mut in_track = false;

    buf.clear();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                path.push(name.clone());
                current_text.clear();

                match name.as_str() {
                    "artists" => {
                        if in_track {
                            in_track_artists = true;
                        } else if !in_tracklist {
                            in_artists = true;
                            artist_position = 0;
                        }
                    }
                    "extraartists" => {
                        if in_track {
                            in_track_extraartists = true;
                        } else {
                            in_extraartists = true;
                            // Don't reset position for extra artists
                        }
                    }
                    "artist" => {
                        if in_track_artists || in_track_extraartists {
                            current_track_artist = TrackArtist::default();
                        } else if in_artists || in_extraartists {
                            artist_position += 1;
                            current_artist = ReleaseArtist::default();
                            current_artist.position = artist_position;
                        }
                    }
                    "tracklist" => {
                        in_tracklist = true;
                    }
                    "track" => {
                        in_track = true;
                        current_track = ReleaseTrack::default();
                    }
                    "label" => {
                        // Labels are empty elements with attributes
                        let mut label = ReleaseLabel::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => label.name = attr.unescape_value()?.to_string(),
                                b"catno" => label.catno = attr.unescape_value()?.to_string(),
                                _ => {}
                            }
                        }
                        release.labels.push(label);
                    }
                    "format" => {
                        let mut format = Format::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => format.name = attr.unescape_value()?.to_string(),
                                b"qty" => {
                                    let val = attr.unescape_value()?;
                                    format.qty = val.parse().unwrap_or(1);
                                }
                                _ => {}
                            }
                        }
                        release.formats.push(format);
                    }
                    "image" => {
                        let mut image = ReleaseImage::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"type" => image.image_type = attr.unescape_value()?.to_string(),
                                b"width" => {
                                    let val = attr.unescape_value()?;
                                    image.width = val.parse().unwrap_or(0);
                                }
                                b"height" => {
                                    let val = attr.unescape_value()?;
                                    image.height = val.parse().unwrap_or(0);
                                }
                                b"uri" => image.uri = attr.unescape_value()?.to_string(),
                                _ => {}
                            }
                        }
                        release.images.push(image);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "label" => {
                        let mut label = ReleaseLabel::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => label.name = attr.unescape_value()?.to_string(),
                                b"catno" => label.catno = attr.unescape_value()?.to_string(),
                                _ => {}
                            }
                        }
                        release.labels.push(label);
                    }
                    "format" => {
                        let mut format = Format::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => format.name = attr.unescape_value()?.to_string(),
                                b"qty" => {
                                    let val = attr.unescape_value()?;
                                    format.qty = val.parse().unwrap_or(1);
                                }
                                _ => {}
                            }
                        }
                        release.formats.push(format);
                    }
                    "image" => {
                        let mut image = ReleaseImage::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"type" => image.image_type = attr.unescape_value()?.to_string(),
                                b"width" => {
                                    let val = attr.unescape_value()?;
                                    image.width = val.parse().unwrap_or(0);
                                }
                                b"height" => {
                                    let val = attr.unescape_value()?;
                                    image.height = val.parse().unwrap_or(0);
                                }
                                b"uri" => image.uri = attr.unescape_value()?.to_string(),
                                _ => {}
                            }
                        }
                        release.images.push(image);
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
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();

                match name.as_str() {
                    "release" => {
                        buf.clear();
                        return Ok(release);
                    }
                    "artists" => {
                        if in_track {
                            in_track_artists = false;
                        } else {
                            in_artists = false;
                        }
                    }
                    "extraartists" => {
                        if in_track {
                            in_track_extraartists = false;
                        } else {
                            in_extraartists = false;
                        }
                    }
                    "artist" => {
                        if in_track_artists || in_track_extraartists {
                            current_track.artists.push(current_track_artist.clone());
                            current_track_artist = TrackArtist::default();
                        } else if in_artists {
                            release.artists.push(current_artist.clone());
                            current_artist = ReleaseArtist::default();
                        } else if in_extraartists {
                            release.extra_artists.push(current_artist.clone());
                            current_artist = ReleaseArtist::default();
                        }
                    }
                    "track" => {
                        release.tracks.push(current_track.clone());
                        current_track = ReleaseTrack::default();
                        in_track = false;
                    }
                    "tracklist" => {
                        in_tracklist = false;
                    }
                    // Text content elements
                    "title" => {
                        if in_track {
                            current_track.title = current_text.clone();
                        } else {
                            // Only set release title at release level (not inside nested elements)
                            let parent = path.get(path.len().saturating_sub(2));
                            if parent.is_none_or(|p| p == "release") {
                                release.title = current_text.clone();
                            }
                        }
                    }
                    "country" => {
                        release.country = current_text.clone();
                    }
                    "released" => {
                        release.released = current_text.clone();
                    }
                    "notes" => {
                        release.notes = current_text.clone();
                    }
                    "data_quality" => {
                        release.data_quality = current_text.clone();
                    }
                    "master_id" => {
                        if let Ok(id) = current_text.trim().parse::<u64>() {
                            release.master_id = Some(id);
                        }
                    }
                    "id" => {
                        if in_track_artists || in_track_extraartists {
                            // Track artist ID - we don't use it
                        } else if in_artists || in_extraartists {
                            current_artist.artist_id = current_text.trim().parse().unwrap_or(0);
                        }
                    }
                    "name" => {
                        if in_track_artists || in_track_extraartists {
                            current_track_artist.name = current_text.clone();
                        } else if in_artists || in_extraartists {
                            current_artist.name = current_text.clone();
                        }
                    }
                    "anv" => {
                        if in_artists || in_extraartists {
                            current_artist.anv = current_text.clone();
                        }
                    }
                    "join" => {
                        if in_artists || in_extraartists {
                            current_artist.join_field = current_text.clone();
                        }
                    }
                    "position" => {
                        if in_track {
                            current_track.position = current_text.clone();
                        }
                    }
                    "duration" => {
                        if in_track {
                            current_track.duration = current_text.clone();
                        }
                    }
                    _ => {}
                }

                path.pop();
                current_text.clear();
            }
            Ok(Event::Eof) => {
                return Err(anyhow::anyhow!("Unexpected EOF inside <release>"));
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
    fn test_parse_single_release() {
        let path = fixture_path("single_release.xml");
        let mut releases = Vec::new();
        let count = parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        assert_eq!(count, 1);
        assert_eq!(releases.len(), 1);

        let r = &releases[0];
        assert_eq!(r.id, 1001);
        assert_eq!(r.status, "Accepted");
        assert_eq!(r.title, "OK Computer");
        assert_eq!(r.country, "UK");
        assert_eq!(r.released, "1997-06-16");
        assert_eq!(r.data_quality, "Correct");
        assert_eq!(r.master_id, Some(500));
        assert_eq!(r.format_string(), "CD");

        // Artists
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.artists[0].name, "Radiohead");
        assert_eq!(r.artists[0].artist_id, 1);

        // Extra artists
        assert_eq!(r.extra_artists.len(), 1);
        assert_eq!(r.extra_artists[0].name, "Some Producer");

        // Labels
        assert_eq!(r.labels.len(), 2);
        assert_eq!(r.labels[0].name, "Parlophone");
        assert_eq!(r.labels[0].catno, "7243 8 55229 2 8");
        assert_eq!(r.labels[1].name, "Capitol Records");

        // Tracks
        assert_eq!(r.tracks.len(), 3);
        assert_eq!(r.tracks[0].title, "Airbag");
        assert_eq!(r.tracks[0].position, "1");
        assert_eq!(r.tracks[0].duration, "4:44");
        assert_eq!(r.tracks[1].title, "Paranoid Android");
        assert_eq!(r.tracks[2].title, "Subterranean Homesick Alien");

        // Images
        assert_eq!(r.images.len(), 2);
        assert_eq!(r.images[0].image_type, "primary");
        assert_eq!(
            r.images[0].uri,
            "https://img.discogs.com/abc123/release-1001.jpg"
        );
    }

    #[test]
    fn test_parse_multi_release() {
        let path = fixture_path("multi_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        // Should have multiple releases
        assert!(
            releases.len() >= 3,
            "Expected at least 3 releases, got {}",
            releases.len()
        );

        // Spot-check some fields
        let ids: Vec<u64> = releases.iter().map(|r| r.id).collect();
        assert!(ids.contains(&1002));
        assert!(ids.contains(&2001));
    }

    #[test]
    fn test_skip_release_without_artists() {
        let path = fixture_path("multi_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        // Release 9999 has no artists and should be skipped
        let ids: Vec<u64> = releases.iter().map(|r| r.id).collect();
        assert!(
            !ids.contains(&9999),
            "Release 9999 (no artists) should be skipped"
        );
    }

    #[test]
    fn test_optional_fields() {
        let path = fixture_path("multi_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        // Release 4001 has no master_id
        let r4001 = releases.iter().find(|r| r.id == 4001).unwrap();
        assert_eq!(r4001.master_id, None);
        assert_eq!(r4001.released, "2001-06-05");

        // Release 5002 has no released date
        let r5002 = releases.iter().find(|r| r.id == 5002);
        if let Some(r) = r5002 {
            assert_eq!(r.released, "");
        }
    }

    #[test]
    fn test_unicode_and_entities() {
        let path = fixture_path("multi_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        // Release 6001 has Björk (unicode)
        let r6001 = releases.iter().find(|r| r.id == 6001).unwrap();
        assert_eq!(r6001.artists[0].name, "Björk");

        // Release 9002 has Simon & Garfunkel (entity)
        let r9002 = releases.iter().find(|r| r.id == 9002).unwrap();
        assert_eq!(r9002.artists[0].name, "Simon & Garfunkel");
    }

    #[test]
    fn test_parse_gzipped() {
        let path = fixture_path("releases_fixture.xml.gz");
        let mut releases_gz = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases_gz.push(r)).unwrap();

        let plain_path = fixture_path("releases_fixture.xml");
        let mut releases_plain = Vec::new();
        parse_releases(&plain_path, None, 100_000, |r| releases_plain.push(r)).unwrap();

        assert_eq!(releases_gz.len(), releases_plain.len());
        for (gz, plain) in releases_gz.iter().zip(releases_plain.iter()) {
            assert_eq!(gz.id, plain.id);
            assert_eq!(gz.title, plain.title);
        }
    }

    #[test]
    fn test_limit() {
        let path = fixture_path("releases_fixture.xml");
        let mut releases = Vec::new();
        let count = parse_releases(&path, Some(3), 100_000, |r| releases.push(r)).unwrap();

        assert_eq!(count, 3);
        assert_eq!(releases.len(), 3);
    }
}
