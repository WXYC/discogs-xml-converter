use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use log::info;

use discogs_xml_converter::filter::ArtistFilter;
use discogs_xml_converter::parser::parse_releases;
use discogs_xml_converter::writer::CsvOutput;

/// Convert Discogs XML data dumps to CSV files.
///
/// Produces 6 CSV files compatible with discogs-cache's import_csv.py.
/// Optionally filters to releases by artists in a library file.
#[derive(Parser)]
#[command(name = "discogs-xml-converter")]
#[command(version, about)]
struct Cli {
    /// Path to Discogs releases XML file (plain or .gz)
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

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    // Load artist filter if specified
    let filter = match &cli.library_artists {
        Some(path) => {
            let f = ArtistFilter::from_file(path)?;
            info!("Loaded {} library artists from {}", f.len(), path.display());
            Some(f)
        }
        None => None,
    };

    // Set up CSV output
    let mut csv_output = CsvOutput::new(&cli.output_dir)?;
    info!("Writing CSV output to {}", cli.output_dir.display());

    let mut written: usize = 0;
    let mut filtered: usize = 0;

    // Parse and write
    let total = parse_releases(&cli.input, cli.limit, cli.progress_interval, |release| {
        // Apply artist filter if specified
        if let Some(ref f) = filter {
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
