# discogs-xml-converter

Purpose-built Rust tool for converting Discogs XML data dumps to CSV files compatible with the [discogs-cache](https://github.com/WXYC/discogs-cache) ETL pipeline.

Replaces three Python scripts with a single binary:

| Python script | What it does | How Rust replaces it |
|---|---|---|
| `discogs-xml2db` (external) | XML to CSV | Built-in `quick-xml` parser |
| `fix_csv_newlines.py` | Fix embedded newlines | Rust `csv` crate produces RFC 4180 output |
| `filter_csv.py` | Filter by artist | `--library-artists` flag filters during parsing |

## Usage

```bash
# Convert all releases to CSV (drop-in replacement for discogs-xml2db)
discogs-xml-converter releases.xml.gz --output-dir /path/to/csv/

# Convert and filter to library artists only (replaces xml2db + fix + filter)
discogs-xml-converter releases.xml.gz --output-dir /path/to/filtered/ \
  --library-artists library_artists.txt

# Limit records for testing
discogs-xml-converter releases.xml.gz --output-dir /tmp/test/ --limit 100
```

### Options

| Flag | Description |
|---|---|
| `--output-dir DIR` | Output directory for CSV files (required) |
| `--library-artists FILE` | Filter to releases by artists in this file (one per line) |
| `--limit N` | Stop after N releases |
| `--progress-interval N` | Log progress every N releases (default: 100000) |

Gzipped input is auto-detected by `.gz` extension.

## CSV Output

Produces 6 CSV files:

| File | Key columns |
|---|---|
| `release.csv` | id, status, title, country, released, notes, data_quality, master_id, format |
| `release_artist.csv` | release_id, artist_id, artist_name, extra, anv, position, join_field |
| `release_label.csv` | release_id, label, catno |
| `release_track.csv` | release_id, sequence, position, title, duration |
| `release_track_artist.csv` | release_id, track_sequence, artist_name |
| `release_image.csv` | release_id, type, width, height, uri |

These are consumed by `discogs-cache/scripts/import_csv.py` using `csv.DictReader`.

## Performance

Release processing is parallelized across all CPU cores:

1. A **scanner thread** reads the XML input and finds `<release>` element boundaries by byte scanning
2. A **rayon worker pool** parses XML and performs NFKD artist name normalization in parallel
3. The **main thread** writes matched releases to CSV sequentially, preserving document order

Artist and label XML files are also processed in parallel when both are present in directory mode.

## Building

```bash
cargo build --release
# Binary at target/release/discogs-xml-converter
```

## Testing

```bash
cargo test
```

All tests use hand-written XML fixtures; no external data dumps needed.

## Integration with discogs-cache

Feed the output into the `--csv-dir` pipeline mode:

```bash
# Convert and filter
discogs-xml-converter releases.xml.gz \
  --output-dir /path/to/filtered/ \
  --library-artists library_artists.txt

# Run database build
python scripts/run_pipeline.py \
  --csv-dir /path/to/filtered/ \
  --database-url postgresql://localhost:5432/discogs
```
