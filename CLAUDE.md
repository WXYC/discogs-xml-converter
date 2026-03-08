# Claude Code Instructions for discogs-xml-converter

## Project Overview

Purpose-built Rust tool for converting Discogs XML data dumps to CSV files compatible with the [discogs-cache](https://github.com/WXYC/discogs-cache) ETL pipeline. Replaces three Python scripts (`discogs-xml2db`, `fix_csv_newlines.py`, `filter_csv.py`) with a single binary.

## Architecture

### Modules

- `model.rs` -- Data structures mirroring Discogs XML `<release>` elements
- `parser.rs` -- Pull-based XML parser using `quick-xml`, supports plain and gzipped input; `parse_release_from_bytes()` enables per-release parsing for the parallel pipeline
- `writer.rs` -- CSV output (6 files matching `import_csv.py` contract)
- `filter.rs` -- Artist name normalization (NFKD + strip combining chars) and HashSet filtering; `ArtistFilter` is `Sync` for parallel access
- `main.rs` -- CLI using clap derive; parallel release processing pipeline (scanner thread + rayon worker pool + sequential writer)

### Parallel Processing Pipeline

Release processing uses a three-stage pipeline for multi-core parallelism:

1. **Scanner thread** -- reads the input file, scans for `<release>...</release>` byte boundaries, batches raw byte ranges (256 per batch), sends via bounded channel (capacity 4)
2. **Rayon worker pool** -- receives batches, parses XML from bytes + normalizes/filters artists in parallel using `par_iter()` (order-preserving)
3. **Writer (main thread)** -- writes matched releases to CSV sequentially, preserving XML document order

Artist and label XML files are processed in parallel via `std::thread::scope` when both are present in directory mode.

### CSV Output Contract

The 6 output CSV files must be compatible with `discogs-cache/scripts/import_csv.py`. Headers and column order are defined in `writer.rs`. Changes to the CSV schema require coordinating with discogs-cache.

## Development

### TDD (Required)

All code changes follow test-driven development. No production code without a failing test first.

### Testing

```bash
cargo test          # all tests (unit, integration, oracle, CLI)
cargo test --lib    # unit tests only
```

No external dependencies needed. All fixtures are hand-written and checked in.

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
