# discogs-xml-converter

Purpose-built Rust tool for converting Discogs XML data dumps to CSV files compatible with the [discogs-etl](https://github.com/WXYC/discogs-etl) ETL pipeline.

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

### Direct-to-PostgreSQL mode

Stream releases directly into PostgreSQL, bypassing the CSV round-trip:

```bash
# Stream releases directly into PostgreSQL
discogs-xml-converter /path/to/xml-dumps/ \
  --output-dir /path/to/supplementary/ \
  --library-artists library_artists.txt \
  --database-url postgresql://localhost:5432/discogs

# With custom batch size (default: 10000)
discogs-xml-converter releases.xml.gz \
  --output-dir /tmp/out/ \
  --database-url postgresql://localhost:5432/discogs \
  --batch-size 5000
```

When `--database-url` is provided, releases are streamed into PostgreSQL via COPY instead of being written to CSV files. Supplementary CSVs (artist_alias.csv, label_hierarchy.csv) are still written to `--output-dir`.

### Options

| Flag | Description |
|---|---|
| `--output-dir DIR` | Output directory for CSV files (required) |
| `--library-artists FILE` | Filter to releases by artists in this file (one per line) |
| `--limit N` | Stop after N releases |
| `--progress-interval N` | Log progress every N releases (default: 100000) |
| `--database-url URL` | Stream releases directly into PostgreSQL via COPY |
| `--batch-size N` | Releases to buffer before flushing to PostgreSQL (default: 10000) |

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

These are consumed by `discogs-etl/scripts/import_csv.py` using `csv.DictReader`.

## Performance

Release processing is parallelized across all CPU cores:

1. A **scanner thread** reads the XML input and finds `<release>` element boundaries by byte scanning
2. A **rayon worker pool** parses XML and performs NFKD artist name normalization in parallel
3. The **main thread** writes matched releases to CSV sequentially, preserving document order

Artist and label XML files are also processed in parallel when both are present in directory mode. In directory mode, the release scanner starts before artist/label processing completes, overlapping the large file read with smaller-file CPU work.

### Gzipped input

The converter streams `.xml.gz` files via on-the-fly decompression (flate2 GzDecoder), auto-detected by the `.gz` extension. This reduces disk I/O from ~57GB to ~5GB for the releases file, but adds CPU overhead for decompression. Net win when the bottleneck is disk throughput (slow or external drives); neutral or slightly slower on fast NVMe where CPU is the bottleneck.

## Building

Requires the [Rust toolchain](https://rustup.rs/).

```bash
cargo build --release
cargo install --path .
```

`cargo install` copies the binary to `~/.cargo/bin/discogs-xml-converter`, which is on your PATH if you installed Rust via rustup. Alternatively, reference the binary directly at `target/release/discogs-xml-converter`.

## Testing

```bash
cargo test
```

All tests use hand-written XML fixtures; no external data dumps needed.

## Integration with discogs-etl

### Direct-to-PostgreSQL pipeline (recommended)

Streams releases directly into PostgreSQL, eliminating the CSV round-trip:

```bash
python scripts/run_pipeline.py \
  --xml /path/to/xml-dumps/ \
  --library-artists library_artists.txt \
  --database-url postgresql://localhost:5432/discogs \
  --direct-pg
```

### CSV pipeline

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
