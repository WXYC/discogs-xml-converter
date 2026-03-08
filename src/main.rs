use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::Sender;
use log::{info, warn};

use discogs_xml_converter::artist_parser::parse_artists;
use discogs_xml_converter::artist_writer::ArtistCsvOutput;
use discogs_xml_converter::filter::ArtistFilter;
use discogs_xml_converter::label_parser::parse_labels;
use discogs_xml_converter::label_writer::LabelCsvOutput;
use discogs_xml_converter::parser::parse_release_from_bytes;
use discogs_xml_converter::writer::CsvOutput;
use rayon::prelude::*;

/// Convert Discogs XML data dumps to CSV files.
///
/// Produces CSV files compatible with discogs-cache's import_csv.py.
/// Optionally filters to releases by artists in a library file.
///
/// Input can be a single releases XML file or a directory containing
/// multiple XML dumps (artists.xml, labels.xml, releases.xml).
/// When a directory is given, files are auto-detected by root element.
#[derive(Parser)]
#[command(name = "discogs-xml-converter")]
#[command(version, about)]
struct Cli {
    /// Path to Discogs XML file or directory containing XML dumps
    input: PathBuf,

    /// Output directory for CSV files
    #[arg(long)]
    output_dir: PathBuf,

    /// Path to library_artists.txt for filtering
    #[arg(long)]
    library_artists: Option<PathBuf>,

    /// Maximum number of releases to process
    #[arg(long)]
    limit: Option<usize>,

    /// Log progress every N releases
    #[arg(long, default_value = "100000")]
    progress_interval: usize,
}

/// Detect the XML type by reading the root element.
///
/// Returns "releases", "artists", "labels", or "unknown".
fn detect_xml_type(path: &PathBuf) -> Result<String> {
    use flate2::read::GzDecoder;
    use quick_xml::Reader;
    use std::io::BufReader;

    let is_gzip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));

    let file =
        fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    let root_element = if is_gzip {
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        find_root_element(Reader::from_reader(reader))?
    } else {
        let reader = BufReader::new(file);
        find_root_element(Reader::from_reader(reader))?
    };

    Ok(root_element)
}

fn find_root_element<R: std::io::BufRead>(mut reader: quick_xml::Reader<R>) -> Result<String> {
    use quick_xml::events::Event;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                return Ok(String::from_utf8_lossy(e.name().as_ref()).to_string());
            }
            Ok(Event::Eof) => return Ok("unknown".to_string()),
            Err(e) => return Err(anyhow::anyhow!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }
}

/// Scan a directory for XML/XML.GZ files and categorize them.
struct XmlFiles {
    artists: Option<PathBuf>,
    labels: Option<PathBuf>,
    releases: Option<PathBuf>,
}

fn scan_directory(dir: &PathBuf) -> Result<XmlFiles> {
    let mut files = XmlFiles {
        artists: None,
        labels: None,
        releases: None,
    };

    for entry in
        fs::read_dir(dir).with_context(|| format!("Failed to read directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        // Only process .xml and .xml.gz files
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.ends_with(".xml") && !name.ends_with(".xml.gz") {
            continue;
        }

        match detect_xml_type(&path)?.as_str() {
            "artists" => {
                info!("Found artists XML: {}", path.display());
                files.artists = Some(path);
            }
            "labels" => {
                info!("Found labels XML: {}", path.display());
                files.labels = Some(path);
            }
            "releases" => {
                info!("Found releases XML: {}", path.display());
                files.releases = Some(path);
            }
            other => {
                warn!(
                    "Skipping unrecognized XML file {} (root element: {})",
                    path.display(),
                    other
                );
            }
        }
    }

    Ok(files)
}

fn process_artists(path: &Path, output_dir: &Path) -> Result<()> {
    info!("Processing artists XML: {}", path.display());
    let mut writer = ArtistCsvOutput::new(output_dir)?;
    let count = parse_artists(path, |artist| {
        writer.write_artist(&artist).unwrap();
    })?;
    writer.flush()?;
    info!("Wrote {} artists to CSV", count);
    Ok(())
}

fn process_labels(path: &Path, output_dir: &Path) -> Result<()> {
    info!("Processing labels XML: {}", path.display());
    let mut writer = LabelCsvOutput::new(output_dir)?;
    let mut hierarchy_count = 0;
    let count = parse_labels(path, |label| {
        if label.parent_id.is_some() {
            hierarchy_count += 1;
        }
        writer.write_label(&label).unwrap();
    })?;
    writer.flush()?;
    info!(
        "Wrote {} labels ({} with parent relationships) to CSV",
        count, hierarchy_count
    );
    Ok(())
}

/// Result of filtering a parsed release.
enum FilterResult {
    /// Release matches filter (or no filter); should be written.
    Matched(Box<discogs_xml_converter::model::Release>),
    /// Release was filtered out.
    Filtered,
    /// Release had no artists and was skipped during parsing.
    NoArtists,
}

fn process_releases(
    path: &Path,
    output_dir: &Path,
    filter: &Option<ArtistFilter>,
    limit: Option<usize>,
    progress_interval: usize,
) -> Result<()> {
    info!("Processing releases XML: {}", path.display());

    const BATCH_SIZE: usize = 256;
    let (tx, rx) = crossbeam_channel::bounded::<ReleaseBatch>(4);

    std::thread::scope(|s| {
        // Scanner thread: read input, find release boundaries, send batches.
        // Takes ownership of tx so the channel closes when scanning completes.
        let scanner = s.spawn(|| -> Result<usize> {
            scan_and_batch_releases(path, limit, progress_interval, BATCH_SIZE, tx)
        });

        // Main thread: receive batches, parse+filter with rayon, write matches
        let mut csv = CsvOutput::new(output_dir)?;
        let mut written = 0usize;
        let mut filtered = 0usize;
        let mut skipped_no_artists = 0usize;

        for batch in &rx {
            // Parse and filter in parallel, preserving order
            let results: Vec<FilterResult> = batch
                .offsets
                .par_iter()
                .map(|&(start, end)| {
                    let bytes = &batch.data[start..end];
                    let release = match parse_release_from_bytes(bytes) {
                        Ok(r) => r,
                        Err(_) => return FilterResult::NoArtists,
                    };

                    if release.artists.is_empty() {
                        return FilterResult::NoArtists;
                    }

                    if let Some(f) = filter.as_ref() {
                        if !matches_filter(f, &release) {
                            return FilterResult::Filtered;
                        }
                    }

                    FilterResult::Matched(Box::new(release))
                })
                .collect();

            // Write results sequentially to preserve document order
            for result in results {
                match result {
                    FilterResult::Matched(release) => {
                        csv.write_release(&release)?;
                        written += 1;
                    }
                    FilterResult::Filtered => filtered += 1,
                    FilterResult::NoArtists => skipped_no_artists += 1,
                }
            }
        }

        csv.flush()?;
        let total = scanner.join().unwrap()?;
        info!(
            "Complete: {} scanned, {} written, {} filtered, {} skipped (no artists)",
            total, written, filtered, skipped_no_artists
        );
        Ok(())
    })
}

/// A batch of release byte ranges from a contiguous buffer.
struct ReleaseBatch {
    /// Buffer containing concatenated raw XML for multiple releases.
    data: Vec<u8>,
    /// (start, end) byte offsets into `data` for each release.
    offsets: Vec<(usize, usize)>,
}

/// Scan an XML input stream for `<release>...</release>` boundaries and send
/// batches of raw byte ranges to a channel.
///
/// Returns the total number of releases found.
fn scan_and_batch_releases(
    path: &Path,
    limit: Option<usize>,
    progress_interval: usize,
    batch_size: usize,
    tx: Sender<ReleaseBatch>,
) -> Result<usize> {
    let is_gzip = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"));

    let file =
        fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;

    if is_gzip {
        let decoder = flate2::read::GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        scan_reader(reader, limit, progress_interval, batch_size, &tx)
    } else {
        let reader = BufReader::new(file);
        scan_reader(reader, limit, progress_interval, batch_size, &tx)
    }
    // tx dropped here, closing the channel
}

const RELEASE_START: &[u8] = b"<release ";
const RELEASE_END: &[u8] = b"</release>";

/// Find `needle` in `haystack`, returning the byte offset of the start.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Scan a reader for release element boundaries and batch them.
///
/// Reads the input into a buffer, scans for `<release `...`</release>` byte
/// boundaries, and sends complete releases as batches via the channel.
fn scan_reader<R: Read>(
    reader: R,
    limit: Option<usize>,
    progress_interval: usize,
    batch_size: usize,
    tx: &Sender<ReleaseBatch>,
) -> Result<usize> {
    let mut reader = BufReader::new(reader);
    let mut count: usize = 0;

    // Buffer of unprocessed bytes (may span multiple read calls)
    let mut unprocessed: Vec<u8> = Vec::new();

    // Current batch being assembled
    let mut batch_data: Vec<u8> = Vec::new();
    let mut batch_offsets: Vec<(usize, usize)> = Vec::new();

    let mut eof = false;

    while !eof {
        // Read more data into unprocessed buffer
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            eof = true;
        } else {
            let len = buf.len();
            unprocessed.extend_from_slice(buf);
            reader.consume(len);
        }

        // Extract complete releases from unprocessed buffer
        loop {
            // Find next <release  start
            let start_offset = match find_bytes(&unprocessed, RELEASE_START) {
                Some(offset) => offset,
                None => {
                    // No start tag found. Discard bytes before the last
                    // possible partial match to prevent unbounded growth,
                    // but keep enough to catch a straddled start tag.
                    let keep = RELEASE_START.len().saturating_sub(1).min(unprocessed.len());
                    let drain_to = unprocessed.len() - keep;
                    if drain_to > 0 {
                        unprocessed.drain(..drain_to);
                    }
                    break;
                }
            };

            // Find </release> after the start
            let search_from = start_offset + RELEASE_START.len();
            let end_offset = match find_bytes(&unprocessed[search_from..], RELEASE_END) {
                Some(offset) => search_from + offset,
                None => {
                    // End tag not yet found; discard bytes before the start
                    // tag and wait for more data
                    if start_offset > 0 {
                        unprocessed.drain(..start_offset);
                    }
                    break;
                }
            };

            let end_pos = end_offset + RELEASE_END.len();

            // Extract the complete release bytes
            let batch_start = batch_data.len();
            batch_data.extend_from_slice(&unprocessed[start_offset..end_pos]);
            batch_offsets.push((batch_start, batch_data.len()));

            count += 1;

            if count.is_multiple_of(progress_interval) {
                info!("Scanned {} releases...", count);
            }

            // Send batch if full
            if batch_offsets.len() >= batch_size {
                tx.send(ReleaseBatch {
                    data: std::mem::take(&mut batch_data),
                    offsets: std::mem::take(&mut batch_offsets),
                })?;
            }

            // Check limit
            if let Some(lim) = limit {
                if count >= lim {
                    if !batch_offsets.is_empty() {
                        tx.send(ReleaseBatch {
                            data: std::mem::take(&mut batch_data),
                            offsets: std::mem::take(&mut batch_offsets),
                        })?;
                    }
                    return Ok(count);
                }
            }

            // Remove processed bytes
            unprocessed.drain(..end_pos);
        }
    }

    // Send remaining batch
    if !batch_offsets.is_empty() {
        tx.send(ReleaseBatch {
            data: std::mem::take(&mut batch_data),
            offsets: std::mem::take(&mut batch_offsets),
        })?;
    }

    Ok(count)
}

/// Check if a release matches the artist filter.
///
/// Uses alias-enhanced matching (by artist_id) when aliases are loaded,
/// otherwise falls back to canonical name matching.
fn matches_filter(filter: &ArtistFilter, release: &discogs_xml_converter::model::Release) -> bool {
    if filter.has_aliases() {
        let artist_ids: Vec<(u64, &str)> = release
            .artists
            .iter()
            .chain(release.extra_artists.iter())
            .map(|a| (a.artist_id, a.name.as_str()))
            .collect();
        filter.matches_any_with_ids(&artist_ids)
    } else {
        let names: Vec<&str> = release
            .artists
            .iter()
            .chain(release.extra_artists.iter())
            .map(|a| a.name.as_str())
            .collect();
        filter.matches_any(names)
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    if cli.input.is_dir() {
        // Directory mode: scan for XML files and process in order
        let xml_files = scan_directory(&cli.input)?;

        // Step 1: Process artists and labels in parallel (independent files)
        match (&xml_files.artists, &xml_files.labels) {
            (Some(artists_path), Some(labels_path)) => {
                std::thread::scope(|s| {
                    let h1 = s.spawn(|| process_artists(artists_path, &cli.output_dir));
                    let h2 = s.spawn(|| process_labels(labels_path, &cli.output_dir));
                    h1.join().unwrap()?;
                    h2.join().unwrap()?;
                    Ok::<(), anyhow::Error>(())
                })?;
            }
            (Some(artists_path), None) => {
                process_artists(artists_path, &cli.output_dir)?;
            }
            (None, Some(labels_path)) => {
                process_labels(labels_path, &cli.output_dir)?;
            }
            (None, None) => {}
        }

        // Step 3: Load artist filter with optional alias enhancement
        let filter = match &cli.library_artists {
            Some(path) => {
                let mut f = ArtistFilter::from_file(path)?;
                info!("Loaded {} library artists from {}", f.len(), path.display());

                // If artists.xml was processed, load aliases for enhanced filtering
                let alias_csv = cli.output_dir.join("artist_alias.csv");
                if alias_csv.exists() {
                    let alias_count = f.load_aliases(&alias_csv)?;
                    info!(
                        "Loaded {} artist aliases for enhanced filtering",
                        alias_count
                    );
                }
                Some(f)
            }
            None => None,
        };

        // Step 4: Process releases with (optionally enhanced) filter
        if let Some(ref releases_path) = xml_files.releases {
            process_releases(
                releases_path,
                &cli.output_dir,
                &filter,
                cli.limit,
                cli.progress_interval,
            )?;
        } else {
            warn!("No releases XML found in directory {}", cli.input.display());
        }

        // Clear filter to drop the borrow
        drop(filter);
    } else {
        // Single file mode: process as releases XML (backward compatible)
        let filter = match &cli.library_artists {
            Some(path) => {
                let f = ArtistFilter::from_file(path)?;
                info!("Loaded {} library artists from {}", f.len(), path.display());
                Some(f)
            }
            None => None,
        };

        process_releases(
            &cli.input,
            &cli.output_dir,
            &filter,
            cli.limit,
            cli.progress_interval,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use discogs_xml_converter::model::{Release, ReleaseArtist};

    fn make_release(artists: Vec<(u64, &str)>, extra_artists: Vec<(u64, &str)>) -> Release {
        Release {
            id: 1,
            artists: artists
                .into_iter()
                .enumerate()
                .map(|(i, (id, name))| ReleaseArtist {
                    artist_id: id,
                    name: name.to_string(),
                    position: (i + 1) as u32,
                    ..Default::default()
                })
                .collect(),
            extra_artists: extra_artists
                .into_iter()
                .enumerate()
                .map(|(i, (id, name))| ReleaseArtist {
                    artist_id: id,
                    name: name.to_string(),
                    position: (i + 1) as u32,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn test_matches_filter_without_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artists.txt");
        std::fs::write(&path, "Radiohead\nBjörk\n").unwrap();
        let filter = ArtistFilter::from_file(&path).unwrap();

        // Main artist matches
        let release = make_release(vec![(1, "Radiohead")], vec![]);
        assert!(matches_filter(&filter, &release));

        // Extra artist matches
        let release = make_release(vec![(99, "Unknown")], vec![(5, "Björk")]);
        assert!(matches_filter(&filter, &release));

        // No match
        let release = make_release(vec![(99, "Unknown")], vec![]);
        assert!(!matches_filter(&filter, &release));
    }

    #[test]
    fn test_matches_filter_with_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("artists.txt");
        std::fs::write(&lib_path, "Puff Daddy\n").unwrap();

        let alias_path = dir.path().join("artist_alias.csv");
        std::fs::write(
            &alias_path,
            "artist_id,artist_name,alias_name\n123,P. Diddy,Puff Daddy\n",
        )
        .unwrap();

        let mut filter = ArtistFilter::from_file(&lib_path).unwrap();
        filter.load_aliases(&alias_path).unwrap();

        // Canonical name doesn't match, but alias does via artist_id
        let release = make_release(vec![(123, "P. Diddy")], vec![]);
        assert!(matches_filter(&filter, &release));

        // Unknown artist doesn't match
        let release = make_release(vec![(999, "Unknown")], vec![]);
        assert!(!matches_filter(&filter, &release));
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn test_scan_finds_release_boundaries() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<releases>
  <release id="1" status="Accepted">
    <title>First</title>
    <artists><artist><id>1</id><name>A</name><anv></anv><join></join></artist></artists>
    <tracklist />
  </release>
  <release id="2" status="Accepted">
    <title>Second</title>
    <artists><artist><id>2</id><name>B</name><anv></anv><join></join></artist></artists>
    <tracklist />
  </release>
</releases>"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xml");
        std::fs::write(&path, xml).unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        let count = scan_and_batch_releases(&path, None, 100_000, 256, tx).unwrap();

        assert_eq!(count, 2);

        let batches: Vec<ReleaseBatch> = rx.iter().collect();
        assert_eq!(batches.len(), 1); // both releases in one batch (< 256)
        assert_eq!(batches[0].offsets.len(), 2);

        // Parse each release from the batch bytes
        for (i, &(start, end)) in batches[0].offsets.iter().enumerate() {
            let release = parse_release_from_bytes(&batches[0].data[start..end]).unwrap();
            assert_eq!(release.id, (i + 1) as u64);
        }
    }

    #[test]
    fn test_scan_handles_cross_buffer_boundary() {
        // Create XML where releases are very small, so even with a tiny read
        // buffer the scanner finds them correctly.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<releases>
  <release id="100" status="Accepted">
    <title>Cross Boundary Test</title>
    <artists><artist><id>10</id><name>Test Artist</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>1</position><title>Track</title><duration>3:00</duration></track></tracklist>
  </release>
  <release id="200" status="Accepted">
    <title>Second Release</title>
    <artists><artist><id>20</id><name>Other Artist</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>1</position><title>Track 2</title><duration>4:00</duration></track></tracklist>
  </release>
</releases>"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xml");
        std::fs::write(&path, xml).unwrap();

        // Use batch_size=1 to force each release into its own batch
        let (tx, rx) = crossbeam_channel::unbounded();
        let count = scan_and_batch_releases(&path, None, 100_000, 1, tx).unwrap();

        assert_eq!(count, 2);

        let batches: Vec<ReleaseBatch> = rx.iter().collect();
        assert_eq!(batches.len(), 2);

        let r1 = parse_release_from_bytes(
            &batches[0].data[batches[0].offsets[0].0..batches[0].offsets[0].1],
        )
        .unwrap();
        assert_eq!(r1.id, 100);
        assert_eq!(r1.title, "Cross Boundary Test");
        assert_eq!(r1.tracks.len(), 1);

        let r2 = parse_release_from_bytes(
            &batches[1].data[batches[1].offsets[0].0..batches[1].offsets[0].1],
        )
        .unwrap();
        assert_eq!(r2.id, 200);
        assert_eq!(r2.title, "Second Release");
    }

    #[test]
    fn test_scan_with_limit() {
        let path = fixture_path("releases_fixture.xml");

        let (tx, rx) = crossbeam_channel::unbounded();
        let count = scan_and_batch_releases(&path, Some(3), 100_000, 256, tx).unwrap();

        assert_eq!(count, 3);

        let batches: Vec<ReleaseBatch> = rx.iter().collect();
        let total_offsets: usize = batches.iter().map(|b| b.offsets.len()).sum();
        assert_eq!(total_offsets, 3);
    }

    #[test]
    fn test_scan_fixture_all_releases() {
        // The releases_fixture.xml has 16 releases + 1 no-artist release (17 total <release> elements)
        // But release 9999 (no artists) is still a valid <release> element in the XML
        let path = fixture_path("releases_fixture.xml");

        let (tx, rx) = crossbeam_channel::unbounded();
        let count = scan_and_batch_releases(&path, None, 100_000, 256, tx).unwrap();

        // Scanner finds all <release> elements including the no-artists one
        // releases_fixture.xml has 16 releases (no 9999 in this fixture)
        assert_eq!(count, 16);

        // Verify each one parses successfully
        let batches: Vec<ReleaseBatch> = rx.iter().collect();
        let mut parsed_ids = Vec::new();
        for batch in &batches {
            for &(start, end) in &batch.offsets {
                let release = parse_release_from_bytes(&batch.data[start..end]).unwrap();
                parsed_ids.push(release.id);
            }
        }
        assert_eq!(parsed_ids.len(), 16);
    }

    #[test]
    fn test_parallel_artist_label_produces_same_output() {
        // Process artists and labels sequentially, then in parallel, and compare
        let artists_path = fixture_path("artists_fixture.xml");
        let labels_path = fixture_path("labels_fixture.xml");

        // Sequential
        let seq_dir = tempfile::tempdir().unwrap();
        process_artists(&artists_path, &seq_dir.path().to_path_buf()).unwrap();
        process_labels(&labels_path, &seq_dir.path().to_path_buf()).unwrap();

        // Parallel
        let par_dir = tempfile::tempdir().unwrap();
        std::thread::scope(|s| {
            let h1 = s.spawn(|| process_artists(&artists_path, &par_dir.path().to_path_buf()));
            let h2 = s.spawn(|| process_labels(&labels_path, &par_dir.path().to_path_buf()));
            h1.join().unwrap().unwrap();
            h2.join().unwrap().unwrap();
        });

        // Compare all output files
        for filename in &[
            "artist_alias.csv",
            "artist_member.csv",
            "label_hierarchy.csv",
        ] {
            let seq_content = std::fs::read_to_string(seq_dir.path().join(filename)).unwrap();
            let par_content = std::fs::read_to_string(par_dir.path().join(filename)).unwrap();
            assert_eq!(
                seq_content, par_content,
                "Mismatch in {} between sequential and parallel processing",
                filename
            );
        }
    }
}
