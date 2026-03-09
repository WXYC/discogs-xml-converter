# Claude Code Instructions for discogs-xml-converter

## Project Overview

Purpose-built Rust tool for converting Discogs XML data dumps to CSV files compatible with the [discogs-cache](https://github.com/WXYC/discogs-cache) ETL pipeline. Replaces three Python scripts (`discogs-xml2db`, `fix_csv_newlines.py`, `filter_csv.py`) with a single binary.

## Architecture

### Modules

- `model.rs` -- Data structures mirroring Discogs XML `<release>` elements
- `parser.rs` -- Pull-based XML parser using `quick-xml`, supports plain and gzipped input; `parse_release_from_bytes()` enables per-release parsing for the parallel pipeline
- `output.rs` -- `ReleaseOutput` trait abstracting over output targets (CSV or PostgreSQL)
- `writer.rs` -- `CsvOutput` implementation of `ReleaseOutput` (6 CSV files matching `import_csv.py` contract)
- `pg_output.rs` -- `PgOutput` implementation of `ReleaseOutput` for direct-to-PostgreSQL streaming via COPY; also contains pure transform functions ported from `import_csv.py` (extract_year, COPY TEXT escaping, artwork selection, dedup)
- `filter.rs` -- Artist name normalization (NFKD + strip combining chars) and HashSet filtering; `ArtistFilter` is `Sync` for parallel access
- `main.rs` -- CLI using clap derive; parallel release processing pipeline (scanner thread + rayon worker pool + sequential writer); output dispatch between CSV and PG modes

### Parallel Processing Pipeline

Release processing uses a three-stage pipeline for multi-core parallelism:

1. **Scanner thread** -- reads the input file, scans for `<release>...</release>` byte boundaries, batches raw byte ranges (256 per batch), sends via bounded channel (capacity 4)
2. **Rayon worker pool** -- receives batches, parses XML from bytes + normalizes/filters artists in parallel using `par_iter()` (order-preserving)
3. **Writer (main thread)** -- writes matched releases via `ReleaseOutput` trait, preserving XML document order

The writer stage dispatches to either `CsvOutput` (CSV files) or `PgOutput` (PostgreSQL COPY) based on the `--database-url` flag. Artist and label XML files are processed in parallel via `std::thread::scope` when both are present in directory mode.

### Output Architecture

The `ReleaseOutput` trait (`output.rs`) provides a common interface for writing release data:

- `write_release()` -- buffer a single release and all its child records
- `flush()` -- send buffered data to the output target
- `finish()` -- flush remaining data and perform post-processing

`CsvOutput` writes 6 CSV files to disk. `PgOutput` buffers COPY TEXT rows in memory and flushes to PostgreSQL every `--batch-size` releases, writing tables in FK order (release first, then children). `PgOutput::finish()` also handles artwork URL population, track count table creation, and cache_metadata insertion.

### CSV Output Contract

The 6 output CSV files must be compatible with `discogs-cache/scripts/import_csv.py`. Headers and column order are defined in `writer.rs`. Changes to the CSV schema require coordinating with discogs-cache.

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

- Artist normalization (`filter.rs:normalize_artist`) must exactly match `discogs-cache/scripts/filter_csv.py:normalize_artist()` -- NFKD decomposition, strip combining characters, lowercase, trim
- Releases with no `<artists>` are skipped (not written to any CSV)
- Format string: single format uses name; qty > 1 prefixes with `{qty}x`; multiple formats are comma-separated
- Track sequence is 1-indexed position in the `<tracklist>`
- Both main and extra track artists go to `release_track_artist.csv` (no `extra` column in that table)
- `parse_release_from_bytes()` enables per-release XML parsing for the parallel pipeline; `extract_release_attrs()` is shared between single-stream and per-release parsers
- The byte scanner finds `<release>` boundaries by searching for `b"<release "` (trailing space distinguishes from `<released>`) and `b"</release>"` (no suffix distinguishes from `</released>`)
- `par_iter().map().collect()` preserves input order so CSV output is deterministic regardless of thread scheduling
- Bounded channel (capacity 4 batches of 256 releases) provides backpressure to prevent unbounded memory growth
- `PgOutput` replicates `import_csv.py`'s transforms in Rust: `extract_year`, empty-to-NULL, COPY TEXT escaping, dedup by unique key, artwork URL selection
- `PgOutput` flushes tables in FK order (release first, then children) so FK constraints are satisfied within each flush
- In direct-PG mode, all tables (including tracks) are imported in a single pass; dedup's CASCADE delete removes extra tracks afterward
- `PgOutput::finish()` handles artwork URLs, `release_track_count` table, and `cache_metadata` -- replicating `import_csv.py --base-only` post-import work
