use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use log::{info, warn};

use discogs_xml_converter::artist_parser::parse_artists;
use discogs_xml_converter::artist_writer::ArtistCsvOutput;
use discogs_xml_converter::filter::ArtistFilter;
use discogs_xml_converter::label_parser::parse_labels;
use discogs_xml_converter::label_writer::LabelCsvOutput;
use discogs_xml_converter::parser::parse_releases;
use discogs_xml_converter::writer::CsvOutput;

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

    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?;

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

    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory {}", dir.display()))?
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
                warn!("Skipping unrecognized XML file {} (root element: {})", path.display(), other);
            }
        }
    }

    Ok(files)
}

fn process_artists(path: &PathBuf, output_dir: &PathBuf) -> Result<()> {
    info!("Processing artists XML: {}", path.display());
    let mut writer = ArtistCsvOutput::new(output_dir)?;
    let count = parse_artists(path, |artist| {
        writer.write_artist(&artist).unwrap();
    })?;
    writer.flush()?;
    info!("Wrote {} artists to CSV", count);
    Ok(())
}

fn process_labels(path: &PathBuf, output_dir: &PathBuf) -> Result<()> {
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

fn process_releases(
    path: &PathBuf,
    output_dir: &PathBuf,
    filter: &Option<ArtistFilter>,
    limit: Option<usize>,
    progress_interval: usize,
) -> Result<()> {
    info!("Processing releases XML: {}", path.display());
    let mut csv_output = CsvOutput::new(output_dir)?;
    let mut written: usize = 0;
    let mut filtered: usize = 0;

    let total = parse_releases(path, limit, progress_interval, |release| {
        if let Some(ref f) = filter {
            if f.has_aliases() {
                // Use alias-enhanced filtering when aliases are loaded
                let artist_ids: Vec<(u64, &str)> = release
                    .artists
                    .iter()
                    .chain(release.extra_artists.iter())
                    .map(|a| (a.artist_id, a.name.as_str()))
                    .collect();

                if !f.matches_any_with_ids(&artist_ids) {
                    filtered += 1;
                    return;
                }
            } else {
                let all_artist_names: Vec<&str> = release
                    .artists
                    .iter()
                    .chain(release.extra_artists.iter())
                    .map(|a| a.name.as_str())
                    .collect();

                if !f.matches_any(all_artist_names) {
                    filtered += 1;
                    return;
                }
            }
        }

        csv_output.write_release(&release).unwrap();
        written += 1;
    })?;

    csv_output.flush()?;
    info!(
        "Complete: {} releases parsed, {} written, {} filtered out",
        total, written, filtered
    );

    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    if cli.input.is_dir() {
        // Directory mode: scan for XML files and process in order
        let xml_files = scan_directory(&cli.input)?;

        // Step 1: Process artists (builds alias CSV)
        if let Some(ref artists_path) = xml_files.artists {
            process_artists(artists_path, &cli.output_dir)?;
        }

        // Step 2: Process labels (builds hierarchy CSV)
        if let Some(ref labels_path) = xml_files.labels {
            process_labels(labels_path, &cli.output_dir)?;
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
