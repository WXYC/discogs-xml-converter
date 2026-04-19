# Claude Code Instructions for discogs-xml-converter

## Project Overview

Purpose-built Rust tool for converting Discogs XML data dumps to CSV files compatible with the [discogs-etl](https://github.com/WXYC/discogs-etl) ETL pipeline. Replaces three Python scripts (`discogs-xml2db`, `fix_csv_newlines.py`, `filter_csv.py`) with a single binary.

## Architecture

### Modules

- `model.rs` -- Data structures mirroring Discogs XML `<release>` elements; implements `wxyc_etl::pg::ImageRef` for `ReleaseImage`
- `parser.rs` -- Pull-based XML parser using `quick-xml`, supports plain and gzipped input; `parse_release_from_bytes()` enables per-release parsing for the parallel pipeline
- `output.rs` -- `ReleaseOutput` trait abstracting over output targets (CSV or PostgreSQL)
- `writer.rs` -- `CsvOutput` implementation of `ReleaseOutput` using `wxyc_etl::csv_writer::MultiCsvWriter` for 9 CSV files matching `import_csv.py` contract
- `pg_output.rs` -- `PgOutput` implementation of `ReleaseOutput` for direct-to-PostgreSQL streaming via COPY; uses `wxyc_etl::pg::BatchCopier` for FK-ordered flush and `wxyc_etl::pg::copy` for COPY TEXT escaping; domain-specific post-import logic (artwork URLs, track counts, cache_metadata) remains local
- `filter.rs` -- `ArtistFilter` HashSet-based artist name filtering with alias support; normalization delegates to `wxyc_etl::text::normalize_artist_name()`
- `main.rs` -- CLI using clap derive; parallel release processing pipeline (scanner thread + rayon worker pool + sequential writer); output dispatch between CSV and PG modes

### Parallel Processing Pipeline

Release processing uses a three-stage pipeline for multi-core parallelism:

1. **Scanner thread** -- reads the input file, scans for `<release>...</release>` byte boundaries using SIMD-accelerated `memchr::memmem`, batches raw byte ranges (256 per batch), sends via bounded channel (capacity 64)
2. **Rayon worker pool** -- receives batches, parses XML from bytes + normalizes/filters artists in parallel using `par_iter()` (order-preserving)
3. **Writer (main thread)** -- writes matched releases via `ReleaseOutput` trait, preserving XML document order

The writer stage dispatches to either `CsvOutput` (CSV files) or `PgOutput` (PostgreSQL COPY) based on the `--database-url` flag. In directory mode, the scanner starts before artist/label processing completes (`start_scanner` + `consume_releases`), overlapping the large file read with smaller-file work. Artist and label XML files are processed in parallel via `std::thread::scope` when both are present.

### Output Architecture

The `ReleaseOutput` trait (`output.rs`) provides a common interface for writing release data:

- `write_release()` -- buffer a single release and all its child records
- `flush()` -- send buffered data to the output target
- `finish()` -- flush remaining data and perform post-processing

`CsvOutput` writes 9 CSV files to disk via `wxyc_etl::csv_writer::MultiCsvWriter`. `PgOutput` uses `wxyc_etl::pg::BatchCopier` to buffer COPY TEXT rows in memory and flush to PostgreSQL every `--batch-size` releases, writing tables in FK order (release first, then children). `PgOutput::finish()` also handles artwork URL population, track count table creation, and cache_metadata insertion.

### CSV Output Contract

The 9 output CSV files must be compatible with `discogs-etl/scripts/import_csv.py`. Headers and column order are defined in `writer.rs`. Changes to the CSV schema require coordinating with discogs-etl.

## Development

### TDD (Required)

All code changes follow test-driven development. No production code without a failing test first.

### Testing

```bash
cargo test          # all tests (unit, integration, oracle, CLI)
cargo test --lib    # unit tests only

# PostgreSQL integration tests (requires a test database)
TEST_DATABASE_URL=postgresql:///discogs_test cargo test pg_output
```

Unit tests and CSV tests use hand-written XML fixtures; no external dependencies needed. PostgreSQL integration tests are gated by the `TEST_DATABASE_URL` environment variable and skip automatically when it is not set.

### Build

```bash
cargo build --release   # produces target/release/discogs-xml-converter
```

### Code Style

- `cargo fmt` for formatting
- `cargo clippy` for linting
- Targets macOS ARM64 and Linux x86_64

## Key Design Decisions

- Artist normalization delegates to `wxyc_etl::text::normalize_artist_name()`, the shared implementation that all WXYC ETL repos use for normalization parity. Parity tests in `filter.rs` verify equivalence.
- Releases with no `<artists>` are skipped (not written to any CSV)
- Format string: single format uses name; qty > 1 prefixes with `{qty}x`; multiple formats are comma-separated
- Track sequence is 1-indexed position in the `<tracklist>`
- Both main and extra track artists go to `release_track_artist.csv` (no `extra` column in that table)
- `parse_release_from_bytes()` enables per-release XML parsing for the parallel pipeline; `extract_release_attrs()` is shared between single-stream and per-release parsers
- The byte scanner finds `<release>` boundaries using `memchr::memmem` (SIMD-accelerated) searching for `b"<release "` (trailing space distinguishes from `<released>`) and `b"</release>"` (no suffix distinguishes from `</released>`)
- `par_iter().map().collect()` preserves input order so CSV output is deterministic regardless of thread scheduling
- Bounded channel (capacity 64 batches of 256 releases) provides backpressure to prevent unbounded memory growth
- In directory mode, `start_scanner()` launches the scanner via `std::thread::spawn` before artist/label processing; `consume_releases()` joins it after the filter is ready. Uses `PathBuf` (not `&Path`) for the `'static` lifetime requirement
- `PgOutput` uses `wxyc_etl::pg` for COPY TEXT escaping, `extract_year`, `pick_artwork_url`, dedup by unique key, and FK-ordered batch flush via `BatchCopier`
- In direct-PG mode, all tables (including tracks) are imported in a single pass; dedup's CASCADE delete removes extra tracks afterward
- `PgOutput::finish()` handles artwork URLs, `release_track_count` table, and `cache_metadata` -- replicating `import_csv.py --base-only` post-import work
