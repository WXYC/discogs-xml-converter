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

/// Decode and entity-unescape the text content of a `Text` event.
///
/// quick-xml 0.40 removed `BytesText::unescape()` (which decoded the raw bytes
/// to a string and then resolved the five predefined XML entities). The reader
/// no longer unescapes `Text` events, and `BytesText::decode()` only decodes
/// bytes -> `str` (it does *not* resolve entities, nor does it normalize EOLs).
/// We therefore reproduce the exact prior behavior here: decode, then run the
/// free `quick_xml::escape::unescape()` (which resolves the predefined entities
/// via `resolve_predefined_entity`, identical to the resolver the old
/// `unescape()` used). We deliberately avoid `xml10_content()`/`xml_content()`,
/// which would add XML EOL normalization that the old `unescape()` never did.
pub(crate) fn unescape_text(e: &quick_xml::events::BytesText<'_>) -> Result<String> {
    let decoded = e.decode()?;
    Ok(quick_xml::escape::unescape(&decoded)?.into_owned())
}

/// Resolve an `Event::GeneralRef` (entity or character reference) to its text.
///
/// quick-xml 0.40 stopped folding entity references into the surrounding `Text`
/// event. A run like `Duke Ellington &amp; John Coltrane` now arrives as three
/// events: `Text("Duke Ellington ")`, `GeneralRef("amp")`, `Text(" John
/// Coltrane")`. Under quick-xml 0.37 the same input produced a single already-
/// unescaped `Text("Duke Ellington & John Coltrane")`. To stay byte-identical we
/// must resolve each `GeneralRef` and append it to the accumulated text. The
/// reference content is the bytes between `&` and `;` (e.g. `amp` or `#38`); we
/// re-wrap it as `&{name};` and reuse the same `quick_xml::escape::unescape()`
/// primitive, which resolves both predefined named entities and numeric
/// character references exactly as the old `unescape()` resolver did.
pub(crate) fn resolve_general_ref(e: &quick_xml::events::BytesRef<'_>) -> Result<String> {
    let name = e.decode()?;
    Ok(quick_xml::escape::unescape(&format!("&{name};"))?.into_owned())
}

/// Decode and entity-unescape an attribute value.
///
/// quick-xml 0.40 deprecated `Attribute::unescape_value()` in favor of
/// `normalized_value()`, but `normalized_value()` (and, in 0.40, the deprecated
/// `unescape_value()` too) applies XML attribute-value whitespace normalization:
/// it collapses `\t`, `\r`, `\n`, and `\r\n` into single U+0020 spaces. The old
/// `unescape_value()` did *not* do that -- it only decoded UTF-8 and resolved
/// the predefined entities. Adopting normalization would silently alter Discogs
/// attribute values (`name`, `catno`, `uri`, ...) that contain embedded
/// whitespace. We reproduce the exact prior, non-normalizing behavior: decode
/// the raw bytes as UTF-8, then `quick_xml::escape::unescape()`.
pub(crate) fn unescape_attr(attr: &quick_xml::events::attributes::Attribute<'_>) -> Result<String> {
    let decoded = std::str::from_utf8(&attr.value)?;
    Ok(quick_xml::escape::unescape(decoded)?.into_owned())
}

/// Extract `id` and `status` attributes from a `<release>` start tag.
fn extract_release_attrs(e: &quick_xml::events::BytesStart<'_>) -> Result<(u64, String)> {
    let mut id: u64 = 0;
    let mut status = String::new();
    for attr in e.attributes() {
        let attr = attr?;
        match attr.key.as_ref() {
            b"id" => {
                let val = unescape_attr(&attr)?;
                id = val.parse().unwrap_or(0);
            }
            b"status" => {
                status = unescape_attr(&attr)?;
            }
            _ => {}
        }
    }
    Ok((id, status))
}

/// Parse a single release from a byte slice containing `<release>...</release>`.
///
/// Used by the parallel release processing pipeline to parse individual
/// release elements that have been partitioned from the input stream.
pub fn parse_release_from_bytes(bytes: &[u8]) -> Result<Release> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = Reader::from_reader(BufReader::new(cursor));
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"release" => {
                let (id, status) = extract_release_attrs(e)?;
                let mut inner_buf = Vec::new();
                return parse_release_body(&mut reader, id, status, &mut inner_buf);
            }
            Ok(Event::Eof) => return Err(anyhow::anyhow!("No <release> found in bytes")),
            Err(e) => return Err(anyhow::anyhow!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }
}

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
                let (id, status) = extract_release_attrs(e)?;
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

    // State flags + a pair of depth counters disambiguate elements without
    // tracking the full XML path. `track_depth`/`in_video` route <title>
    // to the right struct; `depth` (relative to the immediate body of
    // <release>; 0 == release-level child) protects `release.title` from
    // being clobbered by stray <title> tags nested inside <notes>, format
    // descriptions, or any other release-level container that happens to
    // embed HTML-like markup. See WXYC/discogs-xml-converter#56.
    // `track_depth` (tracked separately from the release-body `depth`,
    // since it only ever moves on `<track>` open/close) handles nested
    // `<track>` inside `<sub_tracks>`. See WXYC/discogs-xml-converter#58.
    // A Vec<String> path would add a String allocation per element, which
    // is significant when parsing millions of releases.
    let mut current_text = String::new();
    let mut depth: u32 = 0;

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
    // Track-nesting depth. The Discogs XML schema permits `<track>` to contain
    // `<sub_tracks>` with further `<track>` elements (vinyl side groupings,
    // classical movement breakdowns, "index tracks"). A single `in_track`
    // boolean lost the parent's data when an inner `<track>` opened and
    // dropped trailing parent data when the inner `</track>` closed. Tracking
    // depth instead — with a stack to preserve the outer `current_track` while
    // a sub-track is being parsed — keeps the parent row intact and lets
    // sub-tracks land as sibling rows in `release.tracks`. Same depth-counter
    // family as the release-title fix at this site (#56/#57).
    // See WXYC/discogs-xml-converter#58.
    let mut track_depth: u32 = 0;
    let mut sub_tracks_stack: Vec<ReleaseTrack> = Vec::new();

    // Genres, styles, companies parsing state
    let mut in_genres = false;
    let mut in_styles = false;
    let mut in_companies = false;
    let mut in_company = false;
    let mut current_company = ReleaseCompany::default();

    // Videos parsing state
    let mut in_videos = false;
    let mut in_video = false;
    let mut current_video = ReleaseVideo::default();

    buf.clear();
    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(ref e)) => {
                let qname = e.name();
                let name = qname.as_ref();
                current_text.clear();
                depth += 1;

                match name {
                    b"artists" => {
                        if track_depth > 0 {
                            in_track_artists = true;
                        } else if !in_tracklist {
                            in_artists = true;
                            artist_position = 0;
                        }
                    }
                    b"extraartists" => {
                        if track_depth > 0 {
                            in_track_extraartists = true;
                        } else {
                            in_extraartists = true;
                            // Don't reset position for extra artists
                        }
                    }
                    b"artist" => {
                        if in_track_artists || in_track_extraartists {
                            current_track_artist = TrackArtist::default();
                        } else if in_artists || in_extraartists {
                            artist_position += 1;
                            current_artist = ReleaseArtist::default();
                            current_artist.position = artist_position;
                        }
                    }
                    b"tracklist" => {
                        in_tracklist = true;
                    }
                    b"track" => {
                        // Nested `<track>` (inside `<sub_tracks>`): stash
                        // the outer track on the stack so its accumulated
                        // data isn't clobbered. The inner track parses
                        // into a fresh `ReleaseTrack`; on the inner
                        // `</track>` close the outer is restored.
                        // See WXYC/discogs-xml-converter#58.
                        if track_depth > 0 {
                            sub_tracks_stack.push(std::mem::take(&mut current_track));
                        } else {
                            current_track = ReleaseTrack::default();
                        }
                        track_depth += 1;
                    }
                    b"label" => {
                        // Labels are empty elements with attributes
                        let mut label = ReleaseLabel::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => label.name = unescape_attr(&attr)?,
                                b"catno" => label.catno = unescape_attr(&attr)?,
                                _ => {}
                            }
                        }
                        release.labels.push(label);
                    }
                    b"format" => {
                        let mut format = Format::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => format.name = unescape_attr(&attr)?,
                                b"qty" => {
                                    let val = unescape_attr(&attr)?;
                                    format.qty = val.parse().unwrap_or(1);
                                }
                                _ => {}
                            }
                        }
                        release.formats.push(format);
                    }
                    b"image" => {
                        let mut image = ReleaseImage::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"type" => image.image_type = unescape_attr(&attr)?,
                                b"width" => {
                                    let val = unescape_attr(&attr)?;
                                    image.width = val.parse().unwrap_or(0);
                                }
                                b"height" => {
                                    let val = unescape_attr(&attr)?;
                                    image.height = val.parse().unwrap_or(0);
                                }
                                b"uri" => image.uri = unescape_attr(&attr)?,
                                _ => {}
                            }
                        }
                        release.images.push(image);
                    }
                    b"genres" => {
                        in_genres = true;
                    }
                    b"styles" => {
                        in_styles = true;
                    }
                    b"companies" => {
                        in_companies = true;
                    }
                    b"company" => {
                        if in_companies {
                            in_company = true;
                            current_company = ReleaseCompany::default();
                        }
                    }
                    b"videos" => {
                        in_videos = true;
                    }
                    b"video" => {
                        if in_videos {
                            in_video = true;
                            current_video = ReleaseVideo::default();
                            for attr in e.attributes() {
                                let attr = attr?;
                                match attr.key.as_ref() {
                                    b"src" => current_video.src = unescape_attr(&attr)?,
                                    b"duration" => {
                                        let val = unescape_attr(&attr)?;
                                        current_video.duration = val.parse().ok();
                                    }
                                    b"embed" => {
                                        current_video.embed = unescape_attr(&attr)? != "false";
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let qname = e.name();
                let name = qname.as_ref();
                match name {
                    b"label" => {
                        let mut label = ReleaseLabel::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => label.name = unescape_attr(&attr)?,
                                b"catno" => label.catno = unescape_attr(&attr)?,
                                _ => {}
                            }
                        }
                        release.labels.push(label);
                    }
                    b"format" => {
                        let mut format = Format::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"name" => format.name = unescape_attr(&attr)?,
                                b"qty" => {
                                    let val = unescape_attr(&attr)?;
                                    format.qty = val.parse().unwrap_or(1);
                                }
                                _ => {}
                            }
                        }
                        release.formats.push(format);
                    }
                    b"image" => {
                        let mut image = ReleaseImage::default();
                        for attr in e.attributes() {
                            let attr = attr?;
                            match attr.key.as_ref() {
                                b"type" => image.image_type = unescape_attr(&attr)?,
                                b"width" => {
                                    let val = unescape_attr(&attr)?;
                                    image.width = val.parse().unwrap_or(0);
                                }
                                b"height" => {
                                    let val = unescape_attr(&attr)?;
                                    image.height = val.parse().unwrap_or(0);
                                }
                                b"uri" => image.uri = unescape_attr(&attr)?,
                                _ => {}
                            }
                        }
                        release.images.push(image);
                    }
                    b"video" => {
                        if in_videos {
                            let mut video = ReleaseVideo::default();
                            for attr in e.attributes() {
                                let attr = attr?;
                                match attr.key.as_ref() {
                                    b"src" => video.src = unescape_attr(&attr)?,
                                    b"duration" => {
                                        let val = unescape_attr(&attr)?;
                                        video.duration = val.parse().ok();
                                    }
                                    b"embed" => {
                                        video.embed = unescape_attr(&attr)? != "false";
                                    }
                                    _ => {}
                                }
                            }
                            release.videos.push(video);
                        }
                    }
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
                let qname = e.name();
                let name = qname.as_ref();
                // Decrement first so handlers see the depth of the
                // element being closed (depth-0 == release-level child).
                depth = depth.saturating_sub(1);

                match name {
                    b"release" => {
                        buf.clear();
                        return Ok(release);
                    }
                    b"artists" => {
                        if track_depth > 0 {
                            in_track_artists = false;
                        } else {
                            in_artists = false;
                        }
                    }
                    b"extraartists" => {
                        if track_depth > 0 {
                            in_track_extraartists = false;
                        } else {
                            in_extraartists = false;
                        }
                    }
                    b"artist" => {
                        // WXYC/discogs-etl#218: track-level <artists> and
                        // <extraartists> were both pushed onto
                        // `current_track.artists`, collapsing main-credit and
                        // extra-credit rows into one bucket. Route to the
                        // correct slot based on which flag is set.
                        if in_track_artists {
                            current_track.artists.push(current_track_artist.clone());
                            current_track_artist = TrackArtist::default();
                        } else if in_track_extraartists {
                            current_track
                                .extra_artists
                                .push(current_track_artist.clone());
                            current_track_artist = TrackArtist::default();
                        } else if in_artists {
                            release.artists.push(current_artist.clone());
                            current_artist = ReleaseArtist::default();
                        } else if in_extraartists {
                            release.extra_artists.push(current_artist.clone());
                            current_artist = ReleaseArtist::default();
                        }
                    }
                    b"track" => {
                        // Emit the row regardless of nesting depth — both
                        // parent and sub-tracks become sibling rows in
                        // `release.tracks`. Restructuring sub-tracks into a
                        // separate column / table is a follow-up scoped to
                        // discogs-etl. See WXYC/discogs-xml-converter#58.
                        // Pop the saved outer track when we close an inner
                        // one so subsequent text (a trailing `<duration>`,
                        // late `<artists>`, etc. emitted in the parent
                        // after `</sub_tracks>`) routes back to it; at the
                        // outer close the stack is empty and the pop falls
                        // back to a fresh default.
                        release.tracks.push(std::mem::take(&mut current_track));
                        track_depth = track_depth.saturating_sub(1);
                        current_track = sub_tracks_stack.pop().unwrap_or_default();
                    }
                    b"tracklist" => {
                        in_tracklist = false;
                    }
                    // Text content elements — <title> appears at video,
                    // track, and release scope. Routing priority:
                    // video > track > release-level (most specific first).
                    // The depth gate (depth == 0 after decrement, i.e.
                    // immediate child of <release>) prevents <title>
                    // elements nested anywhere deeper — notes containing
                    // HTML-like markup, format descriptions, identifier
                    // descriptions, etc. — from clobbering release.title.
                    // See WXYC/discogs-xml-converter#56.
                    b"title" => {
                        if in_video {
                            current_video.title = current_text.clone();
                        } else if track_depth > 0 {
                            current_track.title = current_text.clone();
                        } else if depth == 0 {
                            release.title = current_text.clone();
                        }
                    }
                    b"country" => {
                        release.country = current_text.clone();
                    }
                    b"released" => {
                        release.released = current_text.clone();
                    }
                    b"notes" => {
                        release.notes = current_text.clone();
                    }
                    b"data_quality" => {
                        release.data_quality = current_text.clone();
                    }
                    b"master_id" => {
                        if let Ok(id) = current_text.trim().parse::<u64>() {
                            release.master_id = Some(id);
                        }
                    }
                    b"id" => {
                        if in_companies && in_company {
                            current_company.company_id = current_text.trim().parse().unwrap_or(0);
                        } else if in_track_artists || in_track_extraartists {
                            // Track artist ID - we don't use it
                        } else if in_artists || in_extraartists {
                            current_artist.artist_id = current_text.trim().parse().unwrap_or(0);
                        }
                    }
                    b"name" => {
                        if in_companies && in_company {
                            current_company.name = current_text.clone();
                        } else if in_track_artists || in_track_extraartists {
                            current_track_artist.name = current_text.clone();
                        } else if in_artists || in_extraartists {
                            current_artist.name = current_text.clone();
                        }
                    }
                    b"genre" => {
                        if in_genres {
                            release.genres.push(current_text.clone());
                        }
                    }
                    b"genres" => {
                        in_genres = false;
                    }
                    b"style" => {
                        if in_styles {
                            release.styles.push(current_text.clone());
                        }
                    }
                    b"styles" => {
                        in_styles = false;
                    }
                    b"entity_type_name" => {
                        if in_companies && in_company {
                            current_company.entity_type_name = current_text.clone();
                        }
                    }
                    b"entity_type" => {
                        if in_companies && in_company {
                            current_company.entity_type = current_text.trim().parse().unwrap_or(0);
                        }
                    }
                    b"company" => {
                        if in_companies {
                            release.companies.push(current_company.clone());
                            current_company = ReleaseCompany::default();
                            in_company = false;
                        }
                    }
                    b"companies" => {
                        in_companies = false;
                    }
                    b"videos" => {
                        in_videos = false;
                    }
                    b"video" => {
                        if in_videos {
                            release.videos.push(current_video.clone());
                            current_video = ReleaseVideo::default();
                            in_video = false;
                        }
                    }
                    b"anv" => {
                        if in_artists || in_extraartists {
                            current_artist.anv = current_text.clone();
                        }
                    }
                    b"join" => {
                        if in_artists || in_extraartists {
                            current_artist.join_field = current_text.clone();
                        }
                    }
                    b"position" => {
                        if track_depth > 0 {
                            current_track.position = current_text.clone();
                        }
                    }
                    b"duration" => {
                        if track_depth > 0 {
                            current_track.duration = current_text.clone();
                        }
                    }
                    b"role" => {
                        // Capture <role> only inside track-level <extraartists>.
                        // Main <artists> entries have no role. Release-level
                        // extraartists carry the role on `current_artist`
                        // (ReleaseArtist) which we don't emit here.
                        // See WXYC/discogs-etl#218.
                        if in_track_extraartists {
                            current_track_artist.role = current_text.clone();
                        }
                    }
                    _ => {}
                }

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
        assert_eq!(r.title, "Confield");
        assert_eq!(r.country, "UK");
        assert_eq!(r.released, "2001-04-09");
        assert_eq!(r.data_quality, "Correct");
        assert_eq!(r.master_id, Some(500));
        assert_eq!(r.format_string(), "CD");

        // Artists
        assert_eq!(r.artists.len(), 1);
        assert_eq!(r.artists[0].name, "Autechre");
        assert_eq!(r.artists[0].artist_id, 1);

        // Extra artists
        assert_eq!(r.extra_artists.len(), 1);
        assert_eq!(r.extra_artists[0].name, "Some Producer");

        // Labels
        assert_eq!(r.labels.len(), 2);
        assert_eq!(r.labels[0].name, "Warp Records");
        assert_eq!(r.labels[0].catno, "WARPCD128");
        assert_eq!(r.labels[1].name, "Warp Records");

        // Tracks
        assert_eq!(r.tracks.len(), 3);
        assert_eq!(r.tracks[0].title, "VI Scose Poise");
        assert_eq!(r.tracks[0].position, "1");
        assert_eq!(r.tracks[0].duration, "7:11");
        assert_eq!(r.tracks[1].title, "Cfern");
        assert_eq!(r.tracks[2].title, "Pen Expers");

        // Images
        assert_eq!(r.images.len(), 2);
        assert_eq!(r.images[0].image_type, "primary");
        assert_eq!(
            r.images[0].uri,
            "https://img.discogs.com/abc123/release-1001.jpg"
        );

        // Genres
        assert_eq!(r.genres, vec!["Electronic", "Rock"]);

        // Styles
        assert_eq!(
            r.styles,
            vec!["Alternative Rock", "Art Rock", "Experimental"]
        );

        // Companies
        assert_eq!(r.companies.len(), 2);
        assert_eq!(r.companies[0].company_id, 271046);
        assert_eq!(r.companies[0].name, "The Globe Studios");
        assert_eq!(r.companies[0].entity_type, 23);
        assert_eq!(r.companies[0].entity_type_name, "Recorded At");
        assert_eq!(r.companies[1].company_id, 56025);
        assert_eq!(r.companies[1].name, "Abbey Road Studios");
        assert_eq!(r.companies[1].entity_type_name, "Mixed At");
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
        assert_eq!(r4001.released, "1995-11-13");

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

        // Release 6001 has Nilüfer Yanya (unicode)
        let r6001 = releases.iter().find(|r| r.id == 6001).unwrap();
        assert_eq!(r6001.artists[0].name, "Nilüfer Yanya");

        // Release 9002 has Duke Ellington & John Coltrane (entity)
        let r9002 = releases.iter().find(|r| r.id == 9002).unwrap();
        assert_eq!(r9002.artists[0].name, "Duke Ellington & John Coltrane");
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
    fn test_parse_release_from_bytes() {
        let xml = br#"<release id="1001" status="Accepted">
    <title>Confield</title>
    <country>UK</country>
    <released>2001-04-09</released>
    <notes></notes>
    <data_quality>Correct</data_quality>
    <master_id>500</master_id>
    <artists>
      <artist>
        <id>1</id>
        <name>Autechre</name>
        <anv></anv>
        <join></join>
      </artist>
    </artists>
    <labels>
      <label name="Warp Records" catno="WARPCD128" />
    </labels>
    <formats>
      <format name="CD" qty="1" text="" />
    </formats>
    <genres>
      <genre>Electronic</genre>
      <genre>Rock</genre>
    </genres>
    <styles>
      <style>Alternative Rock</style>
    </styles>
    <companies>
      <company>
        <id>271046</id>
        <name>The Globe Studios</name>
        <entity_type>23</entity_type>
        <entity_type_name>Recorded At</entity_type_name>
      </company>
    </companies>
    <tracklist>
      <track>
        <position>1</position>
        <title>VI Scose Poise</title>
        <duration>7:11</duration>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.id, 1001);
        assert_eq!(release.status, "Accepted");
        assert_eq!(release.title, "Confield");
        assert_eq!(release.country, "UK");
        assert_eq!(release.released, "2001-04-09");
        assert_eq!(release.master_id, Some(500));
        assert_eq!(release.artists.len(), 1);
        assert_eq!(release.artists[0].name, "Autechre");
        assert_eq!(release.artists[0].artist_id, 1);
        assert_eq!(release.labels.len(), 1);
        assert_eq!(release.labels[0].name, "Warp Records");
        assert_eq!(release.tracks.len(), 1);
        assert_eq!(release.tracks[0].title, "VI Scose Poise");
        assert_eq!(release.genres, vec!["Electronic", "Rock"]);
        assert_eq!(release.styles, vec!["Alternative Rock"]);
        assert_eq!(release.companies.len(), 1);
        assert_eq!(release.companies[0].company_id, 271046);
        assert_eq!(release.companies[0].name, "The Globe Studios");
        assert_eq!(release.companies[0].entity_type, 23);
        assert_eq!(release.companies[0].entity_type_name, "Recorded At");
    }

    #[test]
    fn test_parse_release_from_bytes_no_release_tag() {
        let xml = b"<notrelease></notrelease>";
        let result = parse_release_from_bytes(xml);
        assert!(result.is_err());
    }

    /// Regression test: stray text content with the word "title" inside
    /// non-`<title>` containers (e.g. `<notes>`) must not bleed into
    /// `release.title`.
    #[test]
    fn test_nested_title_does_not_clobber_release_title() {
        let xml = br#"<release id="42" status="Accepted">
    <title>Real Release Title</title>
    <artists>
      <artist><id>1</id><name>Test</name><anv></anv><join></join></artist>
    </artists>
    <labels />
    <formats>
      <format name="CD" qty="1" text="">
        <descriptions><description>Album</description></descriptions>
      </format>
    </formats>
    <tracklist>
      <track>
        <position>A1</position>
        <title>Track One</title>
        <duration>3:00</duration>
      </track>
      <track>
        <position>A2</position>
        <title>Track Two</title>
        <duration>4:00</duration>
      </track>
    </tracklist>
    <notes>Some notes with title-like content</notes>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.id, 42);
        assert_eq!(release.title, "Real Release Title");
        assert_eq!(release.tracks.len(), 2);
        assert_eq!(release.tracks[0].title, "Track One");
        assert_eq!(release.tracks[1].title, "Track Two");
    }

    /// WXYC/discogs-xml-converter#56: a `<title>` tag nested inside `<notes>`
    /// (e.g. when a curator pasted HTML-like markup describing another work)
    /// must not clobber `release.title`. The `<notes>` body opens depth-1
    /// territory; a `<title>` at that depth is not the release-level title.
    ///
    /// Before the depth-tracking fix, the `!in_tracklist` guard at the
    /// release-title write site fired here and the wrong title landed in
    /// `release.title` — visible in prod as a discogs-cache row whose title
    /// looks suspicious next to its track list.
    #[test]
    fn test_title_inside_notes_does_not_clobber_release_title() {
        let xml = br#"<release id="39218" status="Accepted">
    <title>Disco Not Disco 2</title>
    <artists>
      <artist><id>194</id><name>Various</name><anv></anv><join></join></artist>
    </artists>
    <notes>Originally released as <title>This Is Radio Clash</title> on a 12" in 1981.</notes>
    <tracklist>
      <track>
        <position>A1</position>
        <title>White Horse</title>
        <duration>5:30</duration>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.id, 39218);
        assert_eq!(
            release.title, "Disco Not Disco 2",
            "release.title must be the depth-0 <title>, not the one nested in <notes>"
        );
        assert_eq!(release.tracks.len(), 1);
        assert_eq!(release.tracks[0].title, "White Horse");
    }

    /// WXYC/discogs-xml-converter#56: a `<title>` element appearing inside
    /// any release-level container other than the top level (e.g. inside
    /// `<format>` descriptions or any other deeper element) must not
    /// clobber `release.title` either.
    #[test]
    fn test_title_inside_format_descriptions_does_not_clobber_release_title() {
        let xml = br#"<release id="100" status="Accepted">
    <title>Real Album Title</title>
    <artists>
      <artist><id>1</id><name>Some Artist</name><anv></anv><join></join></artist>
    </artists>
    <formats>
      <format name="CD" qty="1" text="">
        <descriptions>
          <description>Album</description>
          <title>Bogus Format Title</title>
        </descriptions>
      </format>
    </formats>
    <tracklist>
      <track>
        <position>1</position>
        <title>Track One</title>
        <duration>3:00</duration>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.title, "Real Album Title");
        assert_eq!(release.tracks.len(), 1);
        assert_eq!(release.tracks[0].title, "Track One");
    }

    #[test]
    fn test_limit() {
        let path = fixture_path("releases_fixture.xml");
        let mut releases = Vec::new();
        let count = parse_releases(&path, Some(3), 100_000, |r| releases.push(r)).unwrap();

        assert_eq!(count, 3);
        assert_eq!(releases.len(), 3);
    }

    #[test]
    fn test_parse_videos_from_single_release() {
        let path = fixture_path("single_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        let r = &releases[0];
        assert_eq!(r.videos.len(), 2);

        assert_eq!(
            r.videos[0].src,
            "https://www.youtube.com/watch?v=afMHNll9EVM"
        );
        assert_eq!(r.videos[0].title, "Autechre - Cfern");
        assert_eq!(r.videos[0].duration, Some(325));
        assert_eq!(r.videos[0].embed, true);

        assert_eq!(
            r.videos[1].src,
            "https://www.youtube.com/watch?v=XExCZfMCXdo"
        );
        assert_eq!(r.videos[1].title, "Autechre - VI Scose Poise");
        assert_eq!(r.videos[1].duration, Some(175));
        assert_eq!(r.videos[1].embed, true);
    }

    #[test]
    fn test_parse_video_self_closing() {
        let path = fixture_path("multi_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        // Release 2001 has a self-closing <video> element with embed="false"
        let r2001 = releases.iter().find(|r| r.id == 2001).unwrap();
        assert_eq!(r2001.videos.len(), 1);
        assert_eq!(
            r2001.videos[0].src,
            "https://www.youtube.com/watch?v=selfclose1"
        );
        assert_eq!(r2001.videos[0].duration, Some(209));
        assert_eq!(r2001.videos[0].embed, false);
        assert_eq!(r2001.videos[0].title, "");
    }

    #[test]
    fn test_video_title_does_not_clobber_release_title() {
        let xml = br#"<release id="55" status="Accepted">
    <title>The Real Title</title>
    <artists>
      <artist><id>1</id><name>Test Artist</name><anv></anv><join></join></artist>
    </artists>
    <videos>
      <video src="https://www.youtube.com/watch?v=abc" duration="100" embed="true">
        <title>Video Title Should Not Clobber</title>
        <description></description>
      </video>
    </videos>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.title, "The Real Title");
        assert_eq!(release.videos.len(), 1);
        assert_eq!(release.videos[0].title, "Video Title Should Not Clobber");
    }

    #[test]
    fn test_parse_video_from_bytes() {
        let xml = br#"<release id="77" status="Accepted">
    <title>Some Album</title>
    <artists>
      <artist><id>3</id><name>Some Artist</name><anv></anv><join></join></artist>
    </artists>
    <videos>
      <video src="https://www.youtube.com/watch?v=zzz" duration="200" embed="true">
        <title>Some Video</title>
        <description>desc</description>
      </video>
      <video src="https://www.youtube.com/watch?v=yyy" embed="false" />
    </videos>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.videos.len(), 2);
        assert_eq!(release.videos[0].src, "https://www.youtube.com/watch?v=zzz");
        assert_eq!(release.videos[0].duration, Some(200));
        assert_eq!(release.videos[0].embed, true);
        assert_eq!(release.videos[0].title, "Some Video");
        // Second video has no duration attribute
        assert_eq!(release.videos[1].src, "https://www.youtube.com/watch?v=yyy");
        assert_eq!(release.videos[1].duration, None);
        assert_eq!(release.videos[1].embed, false);
    }

    #[test]
    fn test_releases_without_videos_have_empty_vec() {
        let path = fixture_path("multi_release.xml");
        let mut releases = Vec::new();
        parse_releases(&path, None, 100_000, |r| releases.push(r)).unwrap();

        // Release 1002 has no <videos> section
        let r1002 = releases.iter().find(|r| r.id == 1002).unwrap();
        assert_eq!(r1002.videos.len(), 0);
    }

    /// WXYC/discogs-etl#218: track-level `<extraartists>` are captured
    /// separately from `<artists>`, and the `<role>` element on each
    /// extra-artist entry is preserved on `TrackArtist::role`. Main
    /// `<artists>` entries always have an empty role.
    #[test]
    fn test_parse_track_extra_artists_with_role() {
        let xml = br#"<release id="674529" status="Accepted">
    <title>Live 93</title>
    <artists>
      <artist><id>1</id><name>The Orb</name><anv></anv><join></join></artist>
    </artists>
    <tracklist>
      <track>
        <position>5</position>
        <title>Towers Of Dub</title>
        <duration>10:00</duration>
        <artists>
          <artist><id>1</id><name>The Orb</name><anv></anv><join></join></artist>
        </artists>
        <extraartists>
          <artist>
            <id>10</id>
            <name>Alex Paterson</name>
            <anv></anv>
            <join></join>
            <role>Producer</role>
          </artist>
          <artist>
            <id>11</id>
            <name>Kris Weston</name>
            <anv></anv>
            <join></join>
            <role>Co-Producer</role>
          </artist>
          <artist>
            <id>12</id>
            <name>Thomas Fehlmann</name>
            <anv></anv>
            <join></join>
            <role>Mixed By</role>
          </artist>
        </extraartists>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.tracks.len(), 1);
        let track = &release.tracks[0];

        // Main credit: The Orb, no role.
        assert_eq!(track.artists.len(), 1);
        assert_eq!(track.artists[0].name, "The Orb");
        assert_eq!(track.artists[0].role, "");

        // Extra credits: Paterson/Weston/Fehlmann with role strings.
        assert_eq!(track.extra_artists.len(), 3);
        assert_eq!(track.extra_artists[0].name, "Alex Paterson");
        assert_eq!(track.extra_artists[0].role, "Producer");
        assert_eq!(track.extra_artists[1].name, "Kris Weston");
        assert_eq!(track.extra_artists[1].role, "Co-Producer");
        assert_eq!(track.extra_artists[2].name, "Thomas Fehlmann");
        assert_eq!(track.extra_artists[2].role, "Mixed By");
    }

    /// An empty `<role></role>` element on a track-level extra artist
    /// must yield `role = ""` (not panic, not "empty", not the previous
    /// extra artist's role). Downstream, the empty string is coerced
    /// to NULL by both the PG path (`\N`) and the CSV consumer (per
    /// WXYC/discogs-etl#221).
    #[test]
    fn test_parse_track_extra_artist_empty_role_element() {
        let xml = br#"<release id="1001" status="Accepted">
    <title>Test</title>
    <artists>
      <artist><id>1</id><name>Main Artist</name><anv></anv><join></join></artist>
    </artists>
    <tracklist>
      <track>
        <position>1</position>
        <title>Solo Track</title>
        <duration>3:00</duration>
        <extraartists>
          <artist>
            <id>2</id>
            <name>Some Producer</name>
            <anv></anv>
            <join></join>
            <role></role>
          </artist>
        </extraartists>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        assert_eq!(release.tracks.len(), 1);
        let track = &release.tracks[0];
        assert_eq!(track.extra_artists.len(), 1);
        assert_eq!(track.extra_artists[0].name, "Some Producer");
        // Empty <role></role> element → empty string, NOT "empty" / panic.
        assert_eq!(track.extra_artists[0].role, "");
    }

    /// Release-level `<extraartists>` `<role>` elements must not leak
    /// into track-level state. The track parser only consumes
    /// `<role>` while `in_track_extraartists`.
    #[test]
    fn test_release_extra_artist_role_does_not_pollute_track_state() {
        let xml = br#"<release id="1001" status="Accepted">
    <title>Test</title>
    <artists>
      <artist><id>1</id><name>Main Artist</name><anv></anv><join></join></artist>
    </artists>
    <extraartists>
      <artist>
        <id>2</id>
        <name>Some Producer</name>
        <anv></anv>
        <join></join>
        <role>Producer</role>
      </artist>
    </extraartists>
    <tracklist>
      <track>
        <position>1</position>
        <title>Solo Track</title>
        <duration>3:00</duration>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();
        // Release-level extra artist preserved as before
        assert_eq!(release.extra_artists.len(), 1);
        assert_eq!(release.extra_artists[0].name, "Some Producer");

        // Track has no artists/extra_artists; importantly nothing was
        // accidentally populated by the release-level <role>.
        assert_eq!(release.tracks.len(), 1);
        assert_eq!(release.tracks[0].artists.len(), 0);
        assert_eq!(release.tracks[0].extra_artists.len(), 0);
    }

    /// WXYC/discogs-xml-converter#58: a `<track>` containing `<sub_tracks>`
    /// with further `<track>` elements (used for vinyl side groupings,
    /// classical movement breakdowns, "index tracks") must not corrupt the
    /// parent track row. Before the depth-tracking fix, opening a nested
    /// `<track>` overwrote `current_track` with a fresh default, so the
    /// parent's `position`/`title`/`duration` were lost. Closing the inner
    /// `</track>` pushed the nested row and cleared `in_track` while still
    /// inside the outer `<track>`, dropping any trailing parent-level data
    /// emitted after `</sub_tracks>`.
    ///
    /// Sibling shape: both the parent and its sub-tracks are emitted as
    /// rows in `release.tracks`. Restructuring sub-tracks into their own
    /// table is a follow-up scoped to discogs-etl.
    #[test]
    fn test_sub_tracks_do_not_corrupt_parent_track() {
        // Real Discogs shape: a "side" or grouping track that contains
        // sub_tracks with the actual playable items inside.
        let xml = br#"<release id="1" status="Accepted">
    <title>Some Album</title>
    <artists>
      <artist><id>1</id><name>Some Artist</name><anv></anv><join></join></artist>
    </artists>
    <tracklist>
      <track>
        <position>A</position>
        <title>Side A</title>
        <duration></duration>
        <sub_tracks>
          <track>
            <position>A1</position>
            <title>First Movement</title>
            <duration>5:00</duration>
          </track>
          <track>
            <position>A2</position>
            <title>Second Movement</title>
            <duration>6:00</duration>
          </track>
        </sub_tracks>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();

        // The parent track is preserved with its original position/title.
        let parent = release
            .tracks
            .iter()
            .find(|t| t.position == "A")
            .expect("parent track present");
        assert_eq!(parent.title, "Side A");

        // The sub-tracks are also preserved (with their own positions/titles).
        assert!(
            release
                .tracks
                .iter()
                .any(|t| t.position == "A1" && t.title == "First Movement"),
            "first sub-track A1 present"
        );
        assert!(
            release
                .tracks
                .iter()
                .any(|t| t.position == "A2" && t.title == "Second Movement"),
            "second sub-track A2 present"
        );

        // Exactly three rows: parent + two sub-tracks. Guards against a
        // regression where the inner </track> close pushes a duplicate
        // row at the outer </track> close.
        assert_eq!(release.tracks.len(), 3);
    }

    /// WXYC/discogs-xml-converter#58: parent-track data emitted *after*
    /// `</sub_tracks>` (trailing `<duration>`, `<title>` revisions, or
    /// track-level `<artists>`) must route to the parent, not be dropped
    /// because `in_track` was cleared by an inner `</track>` close.
    #[test]
    fn test_parent_track_data_after_sub_tracks_routes_to_parent() {
        let xml = br#"<release id="2" status="Accepted">
    <title>Compilation</title>
    <artists>
      <artist><id>1</id><name>Various</name><anv></anv><join></join></artist>
    </artists>
    <tracklist>
      <track>
        <position>B</position>
        <title>Side B Suite</title>
        <sub_tracks>
          <track>
            <position>B1</position>
            <title>Part One</title>
            <duration>2:30</duration>
          </track>
        </sub_tracks>
        <duration>10:45</duration>
        <artists>
          <artist><id>42</id><name>Side Composer</name><anv></anv><join></join></artist>
        </artists>
      </track>
    </tracklist>
  </release>"#;

        let release = parse_release_from_bytes(xml).unwrap();

        let parent = release
            .tracks
            .iter()
            .find(|t| t.position == "B")
            .expect("parent track present");
        assert_eq!(parent.title, "Side B Suite");
        // Trailing parent-level <duration> after </sub_tracks> must land on
        // the parent (was previously dropped because in_track was cleared).
        assert_eq!(parent.duration, "10:45");
        // Trailing parent-level <artists> after </sub_tracks> must land on
        // the parent track, not the release.
        assert_eq!(parent.artists.len(), 1);
        assert_eq!(parent.artists[0].name, "Side Composer");

        let child = release
            .tracks
            .iter()
            .find(|t| t.position == "B1")
            .expect("sub-track B1 present");
        assert_eq!(child.title, "Part One");
        assert_eq!(child.duration, "2:30");

        assert_eq!(release.tracks.len(), 2);
    }
}
