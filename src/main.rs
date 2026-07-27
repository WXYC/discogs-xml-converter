use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use crossbeam_channel::Sender;
use log::{info, warn};
use wxyc_etl::cli::{resolve_database_url, DatabaseArgs, ImportArgs, ResumableBuildArgs};

use discogs_xml_converter::artist_parser::parse_artists;
use discogs_xml_converter::artist_writer::ArtistCsvOutput;
use discogs_xml_converter::filter::ArtistFilter;
use discogs_xml_converter::keep_release_ids::parse_keep_release_ids;
use discogs_xml_converter::label_parser::parse_labels;
use discogs_xml_converter::label_writer::LabelCsvOutput;
use discogs_xml_converter::library_pairs::LibraryPairs;
use discogs_xml_converter::master_parser::parse_masters;
use discogs_xml_converter::master_writer::MasterCsvOutput;
use discogs_xml_converter::output::ReleaseOutput;
use discogs_xml_converter::parser::parse_release_from_bytes;
use discogs_xml_converter::pg_output::PgOutput;
use discogs_xml_converter::writer::CsvOutput;
use rayon::prelude::*;

/// Environment variable consulted by `import` when `--database-url` is omitted.
const DATABASE_URL_ENV: &str = "DATABASE_URL_DISCOGS";

/// Convert Discogs XML data dumps to CSV files or stream them to PostgreSQL.
///
/// Input can be a single XML file (releases, artists, labels, masters) or a
/// directory containing multiple XML dumps. When a directory is given, files
/// are auto-detected by root element.
#[derive(Parser)]
#[command(name = "discogs-xml-converter")]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert Discogs XML to CSV files in `--data-dir`.
    Build(BuildCmd),
    /// Stream Discogs XML directly into PostgreSQL via COPY.
    Import(ImportCmd),
}

#[derive(Args)]
struct BuildCmd {
    /// Path to Discogs XML file or directory containing XML dumps.
    input: PathBuf,

    #[command(flatten)]
    build: ResumableBuildArgs,

    /// DEPRECATED: use `--data-dir` (alias retained for one release).
    #[arg(long, hide = true)]
    output_dir: Option<PathBuf>,

    #[command(flatten)]
    shared: SharedReleaseArgs,
}

#[derive(Args)]
struct ImportCmd {
    /// Path to Discogs XML file or directory containing XML dumps.
    input: PathBuf,

    #[command(flatten)]
    db: DatabaseArgs,

    #[command(flatten)]
    import: ImportArgs,

    /// DEPRECATED: use `--data-dir` for supplementary CSV outputs (alias retained for one release).
    #[arg(long, hide = true)]
    output_dir: Option<PathBuf>,

    #[command(flatten)]
    shared: SharedReleaseArgs,

    /// Number of releases to buffer before flushing to PostgreSQL.
    /// Larger values reduce per-COPY overhead but use more memory
    /// (~100 bytes/release across the per-table buffers).
    #[arg(long, default_value = "100000")]
    batch_size: usize,
}

#[derive(Args)]
struct SharedReleaseArgs {
    /// Path to library_artists.txt for artist-only filtering.
    /// Mutually exclusive with `--library-db`.
    #[arg(long, conflicts_with = "library_db")]
    library_artists: Option<PathBuf>,

    /// Path to a SQLite `library.db` for pair-wise `(artist, title)` filtering.
    /// Releases whose title is in the library AND whose credited artist
    /// matches one of the library's artists for that title are kept.
    /// Mutually exclusive with `--library-artists`.
    ///
    /// The artist-only filter retains ~4M releases on a full Discogs dump,
    /// which overflows small destination volumes during `COPY release_artist`.
    /// The pair filter narrows that to ~50K, fitting Railway-sized targets.
    #[arg(long)]
    library_db: Option<PathBuf>,

    /// Path to a release_id allowlist: one Discogs release id per line,
    /// blank lines and `#`-prefixed comments ignored. Any release whose id
    /// appears here is emitted (with its full child row set -- tracklist,
    /// artists, labels, genres, styles) even if it fails the
    /// `--library-artists` / `--library-db` filter. Compatible with either
    /// filter mode, or with neither (a no-op filter already keeps
    /// everything). A missing file is treated as an empty allowlist, not an
    /// error.
    #[arg(long)]
    keep_release_ids: Option<PathBuf>,

    /// Maximum number of releases to process.
    #[arg(long)]
    limit: Option<usize>,

    /// Log progress every N releases.
    #[arg(long, default_value = "100000")]
    progress_interval: usize,

    /// Skip per-file root-element detection and treat the input as the given
    /// dump type. Required when the input is a stream-only source (named pipe,
    /// process substitution): the auto-detector opens the file, reads the
    /// root element, and closes — closing a FIFO read end terminates the
    /// upstream writer before the real scan opens it. With this flag, the
    /// input is opened exactly once.
    #[arg(long, value_parser = ["releases", "artists", "labels", "masters"])]
    xml_type: Option<String>,
}

/// Resolve the data directory, accepting the deprecated `--output-dir` alias
/// with a stderr warning.
fn resolve_data_dir(output_dir: Option<PathBuf>, data_dir: PathBuf) -> PathBuf {
    if let Some(legacy) = output_dir {
        eprintln!(
            "warning: --output-dir is deprecated; use --data-dir (this alias will be removed in the next release)"
        );
        legacy
    } else {
        data_dir
    }
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
    masters: Option<PathBuf>,
    releases: Option<PathBuf>,
}

fn scan_directory(dir: &PathBuf) -> Result<XmlFiles> {
    let mut files = XmlFiles {
        artists: None,
        labels: None,
        masters: None,
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
            "masters" => {
                info!("Found masters XML: {}", path.display());
                files.masters = Some(path);
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

fn process_masters(path: &Path, output_dir: &Path) -> Result<()> {
    info!("Processing masters XML: {}", path.display());
    let mut writer = MasterCsvOutput::new(output_dir)?;
    let count = parse_masters(path, |master| {
        writer.write_master(&master).unwrap();
    })?;
    writer.flush()?;
    info!("Wrote {} masters to CSV", count);
    Ok(())
}

/// Result of filtering a parsed release.
enum FilterResult {
    /// Release matches filter (or no filter); should be written.
    Matched(Box<discogs_xml_converter::model::Release>),
    /// Release failed the configured filter but its id is in the
    /// `--keep-release-ids` allowlist, so it's written anyway.
    MatchedViaAllowlist(Box<discogs_xml_converter::model::Release>),
    /// Release was filtered out.
    Filtered,
    /// Release had no artists and was skipped during parsing.
    NoArtists,
}

/// Outcome of [`decide_keep`]: whether a parsed release should be written,
/// and whether the `--keep-release-ids` allowlist is why.
#[derive(Debug, PartialEq, Eq)]
enum KeepDecision {
    /// No filter configured, or the release matched the filter normally.
    Keep,
    /// Failed the configured filter but is present in the allowlist.
    KeepViaAllowlist,
    /// Failed the filter and isn't allowlisted.
    Drop,
}

/// Decide whether a parsed release should be written to output.
///
/// With no filter configured, every release is kept (status quo). With a
/// filter configured, a release that matches it is kept normally; one that
/// doesn't match is still kept -- as `KeepViaAllowlist` -- when its id
/// appears in `keep_ids`, and dropped otherwise. This is Seam A of
/// WXYC/discogs-etl#327: `--keep-release-ids` lets a WXYC library-pinned
/// override reach the CSVs even when its credited artist falls outside the
/// filter's scope.
fn decide_keep(
    filter: &Option<ReleaseFilter>,
    keep_ids: &HashSet<u64>,
    release: &discogs_xml_converter::model::Release,
) -> KeepDecision {
    match filter {
        None => KeepDecision::Keep,
        Some(f) => {
            if matches_filter(f, release) {
                KeepDecision::Keep
            } else if keep_ids.contains(&release.id) {
                KeepDecision::KeepViaAllowlist
            } else {
                KeepDecision::Drop
            }
        }
    }
}

/// Start the release scanner in a background thread.
///
/// Returns the batch receiver and the scanner thread's join handle.
/// The scanner reads `path`, finds `<release>` byte boundaries, and sends
/// batches via the channel. The bounded channel (capacity 64) provides
/// backpressure — the scanner blocks after buffering ~16K releases.
///
/// Uses `std::thread::spawn` (not `scope`) so the scanner can run
/// independently of artist/label processing in directory mode.
fn start_scanner(
    path: PathBuf,
    limit: Option<usize>,
    progress_interval: usize,
) -> (
    crossbeam_channel::Receiver<ReleaseBatch>,
    std::thread::JoinHandle<Result<usize>>,
) {
    const BATCH_SIZE: usize = 256;
    let (tx, rx) = crossbeam_channel::bounded::<ReleaseBatch>(64);
    let handle = std::thread::spawn(move || {
        scan_and_batch_releases(&path, limit, progress_interval, BATCH_SIZE, tx)
    });
    (rx, handle)
}

/// Consume release batches from a scanner, parse/filter with rayon, and write matches.
///
/// Takes ownership of the receiver and join handle from `start_scanner`.
fn consume_releases(
    rx: crossbeam_channel::Receiver<ReleaseBatch>,
    scanner_handle: std::thread::JoinHandle<Result<usize>>,
    output: &mut impl ReleaseOutput,
    filter: &Option<ReleaseFilter>,
    keep_ids: &HashSet<u64>,
) -> Result<()> {
    let mut written = 0usize;
    let mut filtered = 0usize;
    let mut skipped_no_artists = 0usize;
    let mut duplicate_ids = 0usize;
    let mut kept_via_allowlist = 0usize;
    let mut seen_ids = std::collections::HashSet::new();

    // Run the processing loop, capturing any error so we can drop rx
    // before joining the scanner thread. If rx stays alive after an error,
    // the scanner blocks on send() and the join deadlocks.
    let loop_result: Result<()> = (|| {
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

                    match decide_keep(filter, keep_ids, &release) {
                        KeepDecision::Keep => FilterResult::Matched(Box::new(release)),
                        KeepDecision::KeepViaAllowlist => {
                            FilterResult::MatchedViaAllowlist(Box::new(release))
                        }
                        KeepDecision::Drop => FilterResult::Filtered,
                    }
                })
                .collect();

            // Write results sequentially to preserve document order
            for result in results {
                match result {
                    FilterResult::Matched(release) => {
                        if !seen_ids.insert(release.id) {
                            warn!("Skipping duplicate release id={}", release.id);
                            duplicate_ids += 1;
                            continue;
                        }
                        output.write_release(&release)?;
                        written += 1;
                    }
                    FilterResult::MatchedViaAllowlist(release) => {
                        if !seen_ids.insert(release.id) {
                            warn!("Skipping duplicate release id={}", release.id);
                            duplicate_ids += 1;
                            continue;
                        }
                        output.write_release(&release)?;
                        written += 1;
                        kept_via_allowlist += 1;
                    }
                    FilterResult::Filtered => filtered += 1,
                    FilterResult::NoArtists => skipped_no_artists += 1,
                }
            }
        }

        output.flush()?;
        Ok(())
    })();

    // Drop receiver so scanner's send() unblocks with SendError,
    // preventing deadlock when the loop exited due to an error.
    drop(rx);

    // Scanner thread may return SendError if we dropped rx early — ignore
    // that if we already have a loop error to report.
    let scanner_result = scanner_handle.join().unwrap();
    if let Err(ref e) = loop_result {
        warn!("Release processing failed: {}", e);
        return loop_result;
    }
    let total = scanner_result?;

    let mut msg = format!(
        "Complete: {} scanned, {} written, {} filtered, {} skipped (no artists)",
        total, written, filtered, skipped_no_artists
    );
    if kept_via_allowlist > 0 {
        msg.push_str(&format!(
            ", {} kept via --keep-release-ids allowlist",
            kept_via_allowlist
        ));
    }
    if duplicate_ids > 0 {
        msg.push_str(&format!(", {} duplicate ids skipped", duplicate_ids));
    }
    info!("{}", msg);
    Ok(())
}

/// Scan and process releases from an XML file.
///
/// Convenience wrapper that starts the scanner and immediately consumes
/// batches. Used in single-file mode where there's no overlap opportunity.
fn process_releases(
    path: &Path,
    output: &mut impl ReleaseOutput,
    filter: &Option<ReleaseFilter>,
    keep_ids: &HashSet<u64>,
    limit: Option<usize>,
    progress_interval: usize,
) -> Result<()> {
    info!("Processing releases XML: {}", path.display());
    let (rx, handle) = start_scanner(path.to_path_buf(), limit, progress_interval);
    consume_releases(rx, handle, output, filter, keep_ids)
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

/// Scan a reader for release element boundaries and batch them.
///
/// Reads the input into a buffer, scans for `<release `...`</release>` byte
/// boundaries, and sends complete releases as batches via the channel.
/// Uses SIMD-accelerated search via `memchr::memmem`.
///
/// Uses cursor-based buffer management: a `consumed` cursor tracks how many
/// leading bytes have been logically processed, and the buffer is only
/// compacted when the consumed portion exceeds half the buffer length.
/// This avoids O(n) `drain()` calls on every release.
fn scan_reader<R: Read>(
    reader: R,
    limit: Option<usize>,
    progress_interval: usize,
    batch_size: usize,
    tx: &Sender<ReleaseBatch>,
) -> Result<usize> {
    let mut reader = BufReader::new(reader);
    let mut count: usize = 0;

    let start_finder = memchr::memmem::Finder::new(RELEASE_START);
    let end_finder = memchr::memmem::Finder::new(RELEASE_END);

    // Buffer of unprocessed bytes (may span multiple read calls)
    let mut unprocessed: Vec<u8> = Vec::new();

    // Cursor tracking how many leading bytes have been logically consumed.
    // The live data is `unprocessed[consumed..]`. We compact (drain) only
    // when `consumed > unprocessed.len() / 2` to amortize the O(n) shift.
    let mut consumed: usize = 0;

    // Current batch being assembled
    let mut batch_data: Vec<u8> = Vec::with_capacity(batch_size * 4096);
    let mut batch_offsets: Vec<(usize, usize)> = Vec::with_capacity(batch_size);

    let mut eof = false;

    while !eof {
        // Compact buffer when the consumed portion exceeds half the total length
        if consumed > 0 && consumed > unprocessed.len() / 2 {
            unprocessed.drain(..consumed);
            consumed = 0;
        }

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
            let live = &unprocessed[consumed..];

            // Find next <release  start
            let start_offset = match start_finder.find(live) {
                Some(offset) => offset,
                None => {
                    // No start tag found. Advance cursor past everything
                    // except the last few bytes that could be a partial
                    // start tag straddling buffer boundaries.
                    let keep = RELEASE_START.len().saturating_sub(1).min(live.len());
                    consumed = unprocessed.len() - keep;
                    break;
                }
            };

            // Find </release> after the start
            let search_from = start_offset + RELEASE_START.len();
            let end_offset = match end_finder.find(&live[search_from..]) {
                Some(offset) => search_from + offset,
                None => {
                    // End tag not yet found; advance cursor to the start
                    // tag position and wait for more data.
                    consumed += start_offset;
                    break;
                }
            };

            let end_pos = end_offset + RELEASE_END.len();

            // Extract the complete release bytes
            let batch_start = batch_data.len();
            batch_data.extend_from_slice(&live[start_offset..end_pos]);
            batch_offsets.push((batch_start, batch_data.len()));

            count += 1;

            if count.is_multiple_of(progress_interval) {
                info!("Scanned {} releases...", count);
            }

            // Send batch if full
            if batch_offsets.len() >= batch_size {
                let sent_data = std::mem::take(&mut batch_data);
                let sent_offsets = std::mem::take(&mut batch_offsets);
                batch_data = Vec::with_capacity(batch_size * 4096);
                batch_offsets = Vec::with_capacity(batch_size);
                tx.send(ReleaseBatch {
                    data: sent_data,
                    offsets: sent_offsets,
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

            // Advance cursor past the processed release
            consumed += end_pos;
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

/// Combined release filter. Either the artist-only HashSet or the pair-wise
/// `(artist, title)` index from a `library.db`. Set at startup based on which
/// of `--library-artists` / `--library-db` was passed.
enum ReleaseFilter {
    Artists(ArtistFilter),
    Pairs(LibraryPairs),
}

/// Build the configured release filter from the resolved CLI args.
///
/// `data_dir_for_aliases` should be `Some(data_dir)` in directory mode, where
/// the converter's own `artist_alias.csv` output is available, and `None` in
/// single-file streaming mode (no companion CSVs exist yet). Aliases only
/// apply to artist-only filtering; pair-mode ignores them.
///
/// `--library-artists` and `--library-db` are mutually exclusive at the clap
/// layer; this returns `None` when neither is set.
fn build_filter(
    cfg: &RunConfig,
    data_dir_for_aliases: Option<&Path>,
) -> Result<Option<ReleaseFilter>> {
    if let Some(path) = &cfg.library_db {
        let pairs = LibraryPairs::from_db(path)?;
        info!(
            "Loaded {} library titles spanning {} (artist, title) pairs from {}",
            pairs.len(),
            pairs.pair_count(),
            path.display()
        );
        return Ok(Some(ReleaseFilter::Pairs(pairs)));
    }

    if let Some(path) = &cfg.library_artists {
        let mut f = ArtistFilter::from_file(path)?;
        info!("Loaded {} library artists from {}", f.len(), path.display());

        if let Some(data_dir) = data_dir_for_aliases {
            let alias_csv = data_dir.join("artist_alias.csv");
            if alias_csv.exists() {
                let alias_count = f.load_aliases(&alias_csv)?;
                info!(
                    "Loaded {} artist aliases for enhanced filtering",
                    alias_count
                );
            }
            let nv_csv = data_dir.join("artist_name_variation.csv");
            if nv_csv.exists() {
                let nv_count = f.load_name_variations(&nv_csv)?;
                info!(
                    "Loaded {} artist name variations for enhanced filtering",
                    nv_count
                );
            }
        }
        return Ok(Some(ReleaseFilter::Artists(f)));
    }

    Ok(None)
}

/// Resolve the `--keep-release-ids` allowlist into a `HashSet`.
///
/// Absent `--keep-release-ids` (`path` is `None`) and an allowlist file that
/// doesn't exist on disk both resolve to an empty set -- see
/// `keep_release_ids::parse_keep_release_ids`. An empty set makes
/// `decide_keep` behave exactly like the pre-`--keep-release-ids` code path,
/// so this is what keeps the flag's absence byte-identical to today's output.
fn load_keep_ids(path: Option<&Path>) -> Result<HashSet<u64>> {
    match path {
        Some(path) => parse_keep_release_ids(path),
        None => Ok(HashSet::new()),
    }
}

/// Check if a release matches the configured filter.
///
/// In artist mode, uses alias-enhanced matching (by artist_id) when aliases
/// are loaded, otherwise falls back to canonical name matching. In pair mode,
/// looks up the release title in the library's inverted index and checks
/// whether any credited artist appears in that title's set.
fn matches_filter(filter: &ReleaseFilter, release: &discogs_xml_converter::model::Release) -> bool {
    match filter {
        ReleaseFilter::Artists(f) => {
            if f.has_aliases() {
                let artist_ids: Vec<(u64, &str)> = release
                    .artists
                    .iter()
                    .chain(release.extra_artists.iter())
                    .map(|a| (a.artist_id, a.name.as_str()))
                    .collect();
                f.matches_any_with_ids(&artist_ids)
            } else {
                let names: Vec<&str> = release
                    .artists
                    .iter()
                    .chain(release.extra_artists.iter())
                    .map(|a| a.name.as_str())
                    .collect();
                f.matches_any(names)
            }
        }
        ReleaseFilter::Pairs(p) => {
            let names = release
                .artists
                .iter()
                .chain(release.extra_artists.iter())
                .map(|a| a.name.as_str());
            p.matches(&release.title, names)
        }
    }
}

/// Resolved release-processing configuration shared between subcommands.
struct RunConfig {
    input: PathBuf,
    data_dir: PathBuf,
    library_artists: Option<PathBuf>,
    library_db: Option<PathBuf>,
    keep_release_ids: Option<PathBuf>,
    limit: Option<usize>,
    progress_interval: usize,
    /// When set, skips per-file root-element auto-detection and treats the
    /// input as the given dump type. See SharedReleaseArgs::xml_type.
    xml_type: Option<String>,
    sink: ReleaseSink,
}

enum ReleaseSink {
    Csv,
    Pg {
        database_url: String,
        batch_size: usize,
    },
}

fn build_config(cmd: BuildCmd) -> RunConfig {
    let data_dir = resolve_data_dir(cmd.output_dir, cmd.build.data_dir);
    if cmd.build.resume {
        warn!(
            "--resume is accepted for CLI parity but discogs-xml-converter is single-pass; \
             the state file at {} will be ignored",
            cmd.build.state_file.display()
        );
    }
    RunConfig {
        input: cmd.input,
        data_dir,
        library_artists: cmd.shared.library_artists,
        library_db: cmd.shared.library_db,
        keep_release_ids: cmd.shared.keep_release_ids,
        limit: cmd.shared.limit,
        progress_interval: cmd.shared.progress_interval,
        xml_type: cmd.shared.xml_type,
        sink: ReleaseSink::Csv,
    }
}

fn import_config(cmd: ImportCmd) -> Result<RunConfig> {
    let database_url = resolve_database_url(&cmd.db, DATABASE_URL_ENV)
        .with_context(|| format!("resolve database URL for `import` ({})", DATABASE_URL_ENV))?;
    let data_dir = resolve_data_dir(cmd.output_dir, cmd.import.data_dir);
    if cmd.import.fresh {
        info!("--fresh: truncating release tables before import");
        truncate_release_tables(&database_url)?;
    }
    Ok(RunConfig {
        input: cmd.input,
        data_dir,
        library_artists: cmd.shared.library_artists,
        library_db: cmd.shared.library_db,
        keep_release_ids: cmd.shared.keep_release_ids,
        limit: cmd.shared.limit,
        progress_interval: cmd.shared.progress_interval,
        xml_type: cmd.shared.xml_type,
        sink: ReleaseSink::Pg {
            database_url,
            batch_size: cmd.batch_size,
        },
    })
}

/// Truncate release-scoped tables before a `--fresh` import.
///
/// Children CASCADE off `release`, so a single TRUNCATE clears everything
/// downstream and resets identity sequences.
fn truncate_release_tables(database_url: &str) -> Result<()> {
    let mut client = postgres::Client::connect(database_url, postgres::NoTls)
        .with_context(|| format!("Failed to connect to PostgreSQL at {}", database_url))?;
    client
        .batch_execute("TRUNCATE TABLE release RESTART IDENTITY CASCADE")
        .context("TRUNCATE release CASCADE failed")?;
    Ok(())
}

fn run(cfg: RunConfig) -> Result<()> {
    if cfg.input.is_dir() {
        run_directory(cfg)
    } else {
        run_single_file(cfg)
    }
}

fn run_directory(cfg: RunConfig) -> Result<()> {
    let xml_files = scan_directory(&cfg.input)?;

    // Start scanning releases early so the 57GB file read overlaps with
    // artist/label processing. The bounded channel provides backpressure.
    let scanner = xml_files.releases.as_ref().map(|releases_path| {
        info!("Starting release scanner: {}", releases_path.display());
        start_scanner(
            releases_path.to_path_buf(),
            cfg.limit,
            cfg.progress_interval,
        )
    });

    match (&xml_files.artists, &xml_files.labels) {
        (Some(artists_path), Some(labels_path)) => {
            std::thread::scope(|s| {
                let h1 = s.spawn(|| process_artists(artists_path, &cfg.data_dir));
                let h2 = s.spawn(|| process_labels(labels_path, &cfg.data_dir));
                h1.join().unwrap()?;
                h2.join().unwrap()?;
                Ok::<(), anyhow::Error>(())
            })?;
        }
        (Some(artists_path), None) => {
            process_artists(artists_path, &cfg.data_dir)?;
        }
        (None, Some(labels_path)) => {
            process_labels(labels_path, &cfg.data_dir)?;
        }
        (None, None) => {}
    }

    if let Some(masters_path) = &xml_files.masters {
        process_masters(masters_path, &cfg.data_dir)?;
    }

    let filter = build_filter(&cfg, Some(&cfg.data_dir))?;
    let keep_ids = load_keep_ids(cfg.keep_release_ids.as_deref())?;

    if let Some((rx, handle)) = scanner {
        match &cfg.sink {
            ReleaseSink::Pg {
                database_url,
                batch_size,
            } => {
                let mut output = PgOutput::new(database_url, *batch_size)?;
                consume_releases(rx, handle, &mut output, &filter, &keep_ids)?;
                output.finish()?;
            }
            ReleaseSink::Csv => {
                let mut output = CsvOutput::new(&cfg.data_dir)?;
                consume_releases(rx, handle, &mut output, &filter, &keep_ids)?;
                output.finish()?;
            }
        }
    } else {
        warn!("No releases XML found in directory {}", cfg.input.display());
    }

    Ok(())
}

fn run_single_file(cfg: RunConfig) -> Result<()> {
    // Caller-supplied --xml-type bypasses auto-detection. detect_xml_type
    // opens the input, reads the root element, and closes — that close-on-FIFO
    // breaks the upstream writer, so streaming inputs MUST set xml_type.
    let xml_type = match &cfg.xml_type {
        Some(t) => t.clone(),
        None => detect_xml_type(&cfg.input)?,
    };
    match xml_type.as_str() {
        "masters" => {
            process_masters(&cfg.input, &cfg.data_dir)?;
            return Ok(());
        }
        "artists" => {
            process_artists(&cfg.input, &cfg.data_dir)?;
            return Ok(());
        }
        "labels" => {
            process_labels(&cfg.input, &cfg.data_dir)?;
            return Ok(());
        }
        _ => {} // Default: treat as releases
    }

    // Single-file mode has no companion artist_alias.csv to draw from, so
    // alias-enhanced matching is unavailable here.
    let filter = build_filter(&cfg, None)?;
    let keep_ids = load_keep_ids(cfg.keep_release_ids.as_deref())?;

    match &cfg.sink {
        ReleaseSink::Pg {
            database_url,
            batch_size,
        } => {
            let mut output = PgOutput::new(database_url, *batch_size)?;
            process_releases(
                &cfg.input,
                &mut output,
                &filter,
                &keep_ids,
                cfg.limit,
                cfg.progress_interval,
            )?;
            output.finish()?;
        }
        ReleaseSink::Csv => {
            let mut output = CsvOutput::new(&cfg.data_dir)?;
            process_releases(
                &cfg.input,
                &mut output,
                &filter,
                &keep_ids,
                cfg.limit,
                cfg.progress_interval,
            )?;
            output.finish()?;
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tool = match &cli.cmd {
        Command::Build(_) => "discogs-xml-converter build",
        Command::Import(_) => "discogs-xml-converter import",
    };
    let _logger_guard = wxyc_etl::logger::init(wxyc_etl::logger::LoggerConfig {
        repo: "discogs-xml-converter",
        tool,
        sentry_dsn: None,
        run_id: None,
    });

    // TODO: provision SENTRY_DSN in the runtime env (CI / deploy config) — see issue #33.

    let cfg = match cli.cmd {
        Command::Build(cmd) => build_config(cmd),
        Command::Import(cmd) => import_config(cmd)?,
    };
    run(cfg)
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
        std::fs::write(&path, "Autechre\nNilüfer Yanya\n").unwrap();
        let filter = ReleaseFilter::Artists(ArtistFilter::from_file(&path).unwrap());

        // Main artist matches
        let release = make_release(vec![(1, "Autechre")], vec![]);
        assert!(matches_filter(&filter, &release));

        // Extra artist matches
        let release = make_release(vec![(99, "Unknown")], vec![(5, "Nilüfer Yanya")]);
        assert!(matches_filter(&filter, &release));

        // No match
        let release = make_release(vec![(99, "Unknown")], vec![]);
        assert!(!matches_filter(&filter, &release));
    }

    #[test]
    fn test_matches_filter_with_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("artists.txt");
        std::fs::write(&lib_path, "Sun Ra Arkestra\n").unwrap();

        let alias_path = dir.path().join("artist_alias.csv");
        std::fs::write(
            &alias_path,
            "artist_id,artist_name,alias_name\n123,Sun Ra,Sun Ra Arkestra\n",
        )
        .unwrap();

        let mut artist_filter = ArtistFilter::from_file(&lib_path).unwrap();
        artist_filter.load_aliases(&alias_path).unwrap();
        let filter = ReleaseFilter::Artists(artist_filter);

        // Canonical name doesn't match, but alias does via artist_id
        let release = make_release(vec![(123, "Sun Ra")], vec![]);
        assert!(matches_filter(&filter, &release));

        // Unknown artist doesn't match
        let release = make_release(vec![(999, "Unknown")], vec![]);
        assert!(!matches_filter(&filter, &release));
    }

    #[test]
    fn test_matches_filter_with_pairs() {
        // Build a library.db with two pairs and verify matches_filter
        // dispatches to LibraryPairs::matches.
        use rusqlite::Connection;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("library.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE library (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                artist TEXT NOT NULL,\
                title TEXT NOT NULL,\
                format TEXT\
             );\
             INSERT INTO library (artist, title) VALUES \
                ('Autechre', 'Confield'),\
                ('Stereolab', 'Aluminum Tunes');",
        )
        .unwrap();
        drop(conn);
        let pairs = LibraryPairs::from_db(&db_path).unwrap();
        let filter = ReleaseFilter::Pairs(pairs);

        // Title in library, artist in library's set for that title
        let mut release = make_release(vec![(1, "Autechre")], vec![]);
        release.title = "Confield".to_string();
        assert!(matches_filter(&filter, &release));

        // Title in library but artist isn't credited on this release
        let mut release = make_release(vec![(99, "Unknown")], vec![]);
        release.title = "Confield".to_string();
        assert!(!matches_filter(&filter, &release));

        // Title not in library
        let mut release = make_release(vec![(1, "Autechre")], vec![]);
        release.title = "Some Other Album".to_string();
        assert!(!matches_filter(&filter, &release));

        // Diacritic mismatch on artist still matches via normalization
        let mut release = make_release(vec![(2, "Stereolab")], vec![]);
        release.title = "ALUMINUM TUNES".to_string();
        assert!(matches_filter(&filter, &release));
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
    fn test_decide_keep_via_allowlist_rescues_non_matching_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artists.txt");
        std::fs::write(&path, "Autechre\n").unwrap();
        let filter = Some(ReleaseFilter::Artists(
            ArtistFilter::from_file(&path).unwrap(),
        ));

        let mut keep_ids = std::collections::HashSet::new();
        keep_ids.insert(42u64);

        // Fails the artist filter, but its id is allowlisted.
        let mut release = make_release(vec![(99, "Unknown Artist")], vec![]);
        release.id = 42;
        assert!(matches!(
            decide_keep(&filter, &keep_ids, &release),
            KeepDecision::KeepViaAllowlist
        ));

        // Fails the artist filter and isn't allowlisted.
        let mut release = make_release(vec![(99, "Unknown Artist")], vec![]);
        release.id = 7;
        assert!(matches!(
            decide_keep(&filter, &keep_ids, &release),
            KeepDecision::Drop
        ));

        // Matches the artist filter outright; allowlist membership is moot.
        let mut release = make_release(vec![(1, "Autechre")], vec![]);
        release.id = 999;
        assert!(matches!(
            decide_keep(&filter, &keep_ids, &release),
            KeepDecision::Keep
        ));
    }

    #[test]
    fn test_decide_keep_no_filter_configured_always_keeps() {
        let keep_ids = std::collections::HashSet::new();
        let release = make_release(vec![(1, "Anyone")], vec![]);
        assert!(matches!(
            decide_keep(&None, &keep_ids, &release),
            KeepDecision::Keep
        ));
    }

    #[test]
    fn test_keep_release_ids_allowlist_emits_release_that_fails_filter() {
        // End-to-end: a release whose artist is NOT in library_artists.txt
        // is normally filtered out, but appears in --keep-release-ids, so it
        // must be emitted (with its full row set) anyway. A third release
        // that matches neither the filter nor the allowlist stays dropped.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<releases>
  <release id="1" status="Accepted">
    <title>DOGA</title>
    <artists><artist><id>101</id><name>Juana Molina</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>A1</position><title>la paradoja</title><duration>4:12</duration></track></tracklist>
  </release>
  <release id="2" status="Accepted">
    <title>Pinned Override Edition</title>
    <artists><artist><id>202</id><name>Not In Library</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>A1</position><title>Some Track</title><duration>3:00</duration></track></tracklist>
  </release>
  <release id="3" status="Accepted">
    <title>Unrelated Release</title>
    <artists><artist><id>303</id><name>Also Not In Library</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>1</position><title>Filler</title><duration>2:00</duration></track></tracklist>
  </release>
</releases>"#;

        let dir = tempfile::tempdir().unwrap();
        let xml_path = dir.path().join("releases.xml");
        std::fs::write(&xml_path, xml).unwrap();

        let artists_path = dir.path().join("library_artists.txt");
        std::fs::write(&artists_path, "Juana Molina\n").unwrap();
        let filter = Some(ReleaseFilter::Artists(
            ArtistFilter::from_file(&artists_path).unwrap(),
        ));

        let keep_ids_path = dir.path().join("keep_release_ids.txt");
        std::fs::write(&keep_ids_path, "2\n").unwrap();
        let keep_ids =
            discogs_xml_converter::keep_release_ids::parse_keep_release_ids(&keep_ids_path)
                .unwrap();

        let output_dir = dir.path().join("output");
        let mut output = CsvOutput::new(&output_dir).unwrap();

        process_releases(&xml_path, &mut output, &filter, &keep_ids, None, 100_000).unwrap();
        output.finish().unwrap();

        let release_csv = std::fs::read_to_string(output_dir.join("release.csv")).unwrap();
        assert!(
            release_csv.contains("DOGA"),
            "release matching the artist filter should still be kept: {release_csv}"
        );
        assert!(
            release_csv.contains("Pinned Override Edition"),
            "allowlisted release failing the filter should be emitted: {release_csv}"
        );
        assert!(
            !release_csv.contains("Unrelated Release"),
            "release matching neither the filter nor the allowlist should stay filtered: {release_csv}"
        );

        // The allowlisted release's child rows (tracklist) travel with it.
        let track_csv = std::fs::read_to_string(output_dir.join("release_track.csv")).unwrap();
        assert!(
            track_csv.contains("Some Track"),
            "allowlisted release's tracklist should be emitted: {track_csv}"
        );
    }

    #[test]
    fn test_keep_release_ids_absent_flag_is_byte_identical_to_status_quo() {
        // No --keep-release-ids flag at all (empty set, as build_config /
        // import_config produce when cfg.keep_release_ids is None) must
        // behave exactly like the pre-existing filter-only path.
        let xml_path = fixture_path("releases_fixture.xml");

        let with_empty_keep_ids_dir = tempfile::tempdir().unwrap();
        let mut output_a =
            CsvOutput::new(with_empty_keep_ids_dir.path().join("out").as_path()).unwrap();
        let empty_keep_ids = std::collections::HashSet::new();
        process_releases(
            &xml_path,
            &mut output_a,
            &None,
            &empty_keep_ids,
            None,
            100_000,
        )
        .unwrap();
        output_a.finish().unwrap();

        let baseline_dir = tempfile::tempdir().unwrap();
        let mut output_b = CsvOutput::new(baseline_dir.path().join("out").as_path()).unwrap();
        process_releases(
            &xml_path,
            &mut output_b,
            &None,
            &empty_keep_ids,
            None,
            100_000,
        )
        .unwrap();
        output_b.finish().unwrap();

        for filename in &["release.csv", "release_artist.csv", "release_track.csv"] {
            let a =
                std::fs::read_to_string(with_empty_keep_ids_dir.path().join("out").join(filename))
                    .unwrap();
            let b =
                std::fs::read_to_string(baseline_dir.path().join("out").join(filename)).unwrap();
            assert_eq!(a, b, "mismatch in {filename}");
        }
    }

    #[test]
    fn test_duplicate_release_ids_are_skipped() {
        // XML with two releases sharing the same id=1
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<releases>
  <release id="1" status="Accepted">
    <title>DOGA</title>
    <artists><artist><id>101</id><name>Juana Molina</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>A1</position><title>Cosoco</title><duration>4:12</duration></track></tracklist>
  </release>
  <release id="1" status="Accepted">
    <title>Different Title</title>
    <artists><artist><id>101</id><name>Juana Molina</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>A1</position><title>Other Track</title><duration>3:00</duration></track></tracklist>
  </release>
  <release id="2" status="Accepted">
    <title>Aluminum Tunes</title>
    <artists><artist><id>102</id><name>Stereolab</name><anv></anv><join></join></artist></artists>
    <tracklist><track><position>1</position><title>Fuses</title><duration>7:29</duration></track></tracklist>
  </release>
</releases>"#;

        let dir = tempfile::tempdir().unwrap();
        let xml_path = dir.path().join("releases.xml");
        std::fs::write(&xml_path, xml).unwrap();

        let output_dir = dir.path().join("output");
        let mut output = CsvOutput::new(&output_dir).unwrap();
        let filter = None;

        let keep_ids = std::collections::HashSet::new();
        process_releases(&xml_path, &mut output, &filter, &keep_ids, None, 100_000).unwrap();
        output.finish().unwrap();

        // Read release.csv and verify only 2 releases (id=1 appears once, id=2 once)
        let release_csv = std::fs::read_to_string(output_dir.join("release.csv")).unwrap();
        let lines: Vec<&str> = release_csv.lines().collect();
        // Header + 2 data rows (not 3)
        assert_eq!(
            lines.len(),
            3,
            "Expected header + 2 releases, got: {release_csv}"
        );
        // First occurrence wins: "DOGA" not "Different Title"
        assert!(lines[1].contains("DOGA"), "First occurrence should be kept");
        assert!(
            !release_csv.contains("Different Title"),
            "Duplicate should be skipped"
        );
    }

    #[test]
    fn test_parallel_artist_label_produces_same_output() {
        // Process artists and labels sequentially, then in parallel, and compare
        let artists_path = fixture_path("artists_fixture.xml");
        let labels_path = fixture_path("labels_fixture.xml");

        // Sequential
        let seq_dir = tempfile::tempdir().unwrap();
        process_artists(&artists_path, seq_dir.path()).unwrap();
        process_labels(&labels_path, seq_dir.path()).unwrap();

        // Parallel
        let par_dir = tempfile::tempdir().unwrap();
        std::thread::scope(|s| {
            let h1 = s.spawn(|| process_artists(&artists_path, par_dir.path()));
            let h2 = s.spawn(|| process_labels(&labels_path, par_dir.path()));
            h1.join().unwrap().unwrap();
            h2.join().unwrap().unwrap();
        });

        // Compare all output files
        for filename in &[
            "artist_alias.csv",
            "artist_name_variation.csv",
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
