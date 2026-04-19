//! Oracle tests: verify Rust output matches expected CSVs from discogs-etl.
//!
//! These tests parse releases_fixture.xml and diff each CSV against
//! the expected output copied from discogs-etl/tests/fixtures/csv/.
//!
//! Text normalization is provided by `wxyc_etl::text::normalize_artist_name()`,
//! the shared implementation used by all WXYC ETL repos. The parity tests in
//! `src/filter.rs` verify normalization equivalence.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use discogs_xml_converter::parser::parse_releases;
use discogs_xml_converter::writer::CsvOutput;
use pretty_assertions::assert_eq;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn expected_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("expected")
        .join(name)
}

/// Normalize CSV content for comparison: parse and re-serialize to handle
/// quoting differences, then sort data rows for order-independent comparison.
fn normalize_csv(content: &str) -> String {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(content.as_bytes());

    let headers = rdr.headers().unwrap().clone();
    let mut rows: Vec<Vec<String>> = rdr
        .records()
        .map(|r| {
            let record = r.unwrap();
            record.iter().map(|f| f.to_string()).collect()
        })
        .collect();

    // Sort rows for deterministic comparison
    rows.sort();

    let mut wtr = csv::Writer::from_writer(Vec::new());
    wtr.write_record(&headers).unwrap();
    for row in &rows {
        wtr.write_record(row).unwrap();
    }
    wtr.flush().unwrap();
    String::from_utf8(wtr.into_inner().unwrap()).unwrap()
}

fn generate_output() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let xml_path = fixture_path("releases_fixture.xml");

    let mut csv_output = CsvOutput::new(dir.path()).unwrap();

    parse_releases(&xml_path, None, 100_000, |release| {
        csv_output.write_release(&release).unwrap();
    })
    .unwrap();

    csv_output.flush().unwrap();
    dir
}

#[test]
fn test_oracle_release_csv() {
    let dir = generate_output();
    let actual = fs::read_to_string(dir.path().join("release.csv")).unwrap();
    let expected = fs::read_to_string(expected_path("release.csv")).unwrap();
    assert_eq!(normalize_csv(&actual), normalize_csv(&expected));
}

#[test]
fn test_oracle_release_artist_csv() {
    let dir = generate_output();
    let actual = fs::read_to_string(dir.path().join("release_artist.csv")).unwrap();
    let expected = fs::read_to_string(expected_path("release_artist.csv")).unwrap();
    assert_eq!(normalize_csv(&actual), normalize_csv(&expected));
}

#[test]
fn test_oracle_release_track_csv() {
    let dir = generate_output();
    let actual = fs::read_to_string(dir.path().join("release_track.csv")).unwrap();
    let expected = fs::read_to_string(expected_path("release_track.csv")).unwrap();
    assert_eq!(normalize_csv(&actual), normalize_csv(&expected));
}

#[test]
fn test_oracle_release_track_artist_csv() {
    let dir = generate_output();
    let actual = fs::read_to_string(dir.path().join("release_track_artist.csv")).unwrap();
    let expected = fs::read_to_string(expected_path("release_track_artist.csv")).unwrap();
    assert_eq!(normalize_csv(&actual), normalize_csv(&expected));
}

#[test]
fn test_oracle_release_label_csv() {
    let dir = generate_output();
    let actual = fs::read_to_string(dir.path().join("release_label.csv")).unwrap();
    let expected = fs::read_to_string(expected_path("release_label.csv")).unwrap();
    assert_eq!(normalize_csv(&actual), normalize_csv(&expected));
}

#[test]
fn test_oracle_release_image_csv() {
    let dir = generate_output();
    let actual = fs::read_to_string(dir.path().join("release_image.csv")).unwrap();
    let expected = fs::read_to_string(expected_path("release_image.csv")).unwrap();
    assert_eq!(normalize_csv(&actual), normalize_csv(&expected));
}

#[test]
fn test_referential_integrity() {
    let dir = generate_output();

    // Load release IDs from release.csv
    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let release_ids: HashSet<String> = rdr.records().map(|r| r.unwrap()[0].to_string()).collect();

    // Check all child CSVs
    for (filename, id_col) in &[
        ("release_artist.csv", 0),
        ("release_label.csv", 0),
        ("release_track.csv", 0),
        ("release_track_artist.csv", 0),
        ("release_image.csv", 0),
    ] {
        let path = dir.path().join(filename);
        if !path.exists() {
            continue;
        }
        let mut rdr = csv::Reader::from_path(&path).unwrap();
        for result in rdr.records() {
            let record = result.unwrap();
            let id = &record[*id_col];
            assert!(
                release_ids.contains(id),
                "Orphan release_id {} in {} not found in release.csv",
                id,
                filename
            );
        }
    }
}
