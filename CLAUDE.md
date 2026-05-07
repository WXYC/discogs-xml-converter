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
- `filter.rs` -- `ArtistFilter` HashSet-based artist name filtering with alias support; normalization delegates to `wxyc_etl::text::to_match_form()`
- `library_pairs.rs` -- `LibraryPairs` inverted index `{normalized_title -> set<normalized_artist>}` loaded from a SQLite `library.db`. Powers the `--library-db` pair-wise filter that narrows the converter's ~4M-release artist-only output to ~50K so the import fits Railway-sized destination DBs. Mirrors `discogs-etl/scripts/filter_csv.py::load_library_pairs`
- `main.rs` -- CLI using clap derive with `build` / `import` subcommands; flattens `wxyc_etl::cli::{DatabaseArgs, ResumableBuildArgs, ImportArgs}` for the cache-builder convention; parallel release processing pipeline (scanner thread + rayon worker pool + sequential writer); output dispatch between CSV and PG sinks

### Parallel Processing Pipeline

Release processing uses a three-stage pipeline for multi-core parallelism:

1. **Scanner thread** -- reads the input file, scans for `<release>...</release>` byte boundaries using SIMD-accelerated `memchr::memmem`, batches raw byte ranges (256 per batch), sends via bounded channel (capacity 64)
2. **Rayon worker pool** -- receives batches, parses XML from bytes + normalizes/filters artists in parallel using `par_iter()` (order-preserving)
3. **Writer (main thread)** -- writes matched releases via `ReleaseOutput` trait, preserving XML document order

The writer stage dispatches to either `CsvOutput` (CSV files) or `PgOutput` (PostgreSQL COPY) based on the chosen subcommand: `build` writes CSVs to `--data-dir`, `import` streams to PostgreSQL via `--database-url` (or the `DATABASE_URL_DISCOGS` env fallback resolved through `wxyc_etl::cli::resolve_database_url`). In directory mode, the scanner starts before artist/label processing completes (`start_scanner` + `consume_releases`), overlapping the large file read with smaller-file work. Artist and label XML files are processed in parallel via `std::thread::scope` when both are present.

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

## Observability

`main.rs` initializes `wxyc_etl::logger` at startup. Logs emit as one JSON object per line on stdout; panics and `tracing::error!` events forward to Sentry when `SENTRY_DSN` is set in the environment. Without a DSN, JSON logging still works and Sentry stays inactive.

Sentry tags applied to every event:
- `repo` -- `discogs-xml-converter`
- `tool` -- `discogs-xml-converter`
- `run_id` -- UUIDv4 generated per process invocation
- `step` -- set per-span via `tracing::info_span!("...", step = "...")`

TODO: provision `SENTRY_DSN` in the environments that invoke this tool (discogs-etl GitHub Actions workflow + any manual cache rebuild scripts on EC2). DSN provisioning is tracked separately from this wireup.

## CLI shape (cache-builder convention)

The tool exposes two subcommands that compose shared `wxyc_etl::cli` argument groups:

- `discogs-xml-converter build <input> [--data-dir DIR] [--state-file FILE] [--resume] [--library-artists FILE | --library-db FILE] [--limit N]` — convert XML to CSV files in `--data-dir` (defaults to `./data`). `--resume` is accepted for parity with other cache builders but is currently a no-op (this tool is single-pass).
- `discogs-xml-converter import <input> --database-url URL [--data-dir DIR] [--fresh] [--batch-size N] [--library-artists FILE | --library-db FILE]` — stream releases directly into PostgreSQL via COPY. `--database-url` falls back to the `DATABASE_URL_DISCOGS` environment variable. `--fresh` runs `TRUNCATE release ... CASCADE` before importing.

`--output-dir` is accepted as a deprecation alias for `--data-dir` (with a stderr warning) for one release. The deprecation alias is enforced in tests; remove it in the next breaking-change cycle once all callers (`discogs-etl`'s `run_pipeline.py`) have migrated.

### Filter modes

`--library-artists` and `--library-db` are mutually exclusive. Both run in the streaming scanner — releases are tested as they're parsed, so filtered-out releases never enter the writer's buffers.

- `--library-artists FILE` keeps any release whose credited artist (main or extra) appears in `library_artists.txt`. In directory mode, an `artist_alias.csv` companion enables alias-aware matching by `artist_id`. Yields ~4M releases on a current Discogs dump — too large for Railway-sized destination DBs (overflows during `COPY release_artist`).
- `--library-db FILE` keeps any release whose `(artist, title)` pair appears in the SQLite `library.db` (the `library` table's `artist`, `title` columns). Both sides are normalized via `wxyc_etl::text::to_match_form` so diacritics don't matter. Yields ~50K releases — the size that fits Railway-sized targets and is what `rebuild-cache.sh` runs in production. The pair-filter implementation lives in `src/library_pairs.rs` and is parity-tested against `discogs-etl/scripts/filter_csv.py::filter_csvs_by_pairs` in `tests/parity_test.rs` (opt-in via `DISCOGS_ETL_REPO`).

## Key Design Decisions

- Artist normalization delegates to `wxyc_etl::text::to_match_form()`, the shared implementation that all WXYC ETL repos use for normalization parity. Parity tests in `filter.rs` verify equivalence.
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
