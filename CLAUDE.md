# Claude Code Instructions for discogs-xml-converter

## Project Overview

Purpose-built Rust tool for converting Discogs XML data dumps to CSV files compatible with the [discogs-cache](https://github.com/WXYC/discogs-cache) ETL pipeline. Replaces three Python scripts (`discogs-xml2db`, `fix_csv_newlines.py`, `filter_csv.py`) with a single binary.

## Architecture

### Modules

- `model.rs` -- Data structures mirroring Discogs XML `<release>` elements
- `parser.rs` -- Pull-based XML parser using `quick-xml`, supports plain and gzipped input
- `writer.rs` -- CSV output (6 files matching `import_csv.py` contract)
- `filter.rs` -- Artist name normalization (NFKD + strip combining chars) and HashSet filtering
- `main.rs` -- CLI using clap derive

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
