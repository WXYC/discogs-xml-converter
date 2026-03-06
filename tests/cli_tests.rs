//! CLI integration tests using assert_cmd.
#![allow(deprecated)] // Command::cargo_bin deprecation

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn test_help() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Convert Discogs XML"));
}

#[test]
fn test_missing_required_args() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .assert()
        .failure();
}

#[test]
fn test_missing_output_dir() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(fixture_path("single_release.xml").to_str().unwrap())
        .assert()
        .failure();
}

#[test]
fn test_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(fixture_path("releases_fixture.xml").to_str().unwrap())
        .arg("--output-dir")
        .arg(dir.path().to_str().unwrap())
        .assert()
        .success();

    // Check all 6 CSV files exist
    for filename in &[
        "release.csv",
        "release_artist.csv",
        "release_label.csv",
        "release_track.csv",
        "release_track_artist.csv",
        "release_image.csv",
    ] {
        let path = dir.path().join(filename);
        assert!(path.exists(), "{} should exist", filename);
    }

    // Check row counts
    let count_records = |filename: &str| -> usize {
        let path = dir.path().join(filename);
        let mut rdr = csv::Reader::from_path(&path).unwrap();
        rdr.records().count()
    };

    // releases_fixture.xml has 16 releases, all with artists (none skipped)
    assert_eq!(count_records("release.csv"), 16);
}

#[test]
fn test_with_library_artists_filter() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(fixture_path("releases_fixture.xml").to_str().unwrap())
        .arg("--output-dir")
        .arg(dir.path().to_str().unwrap())
        .arg("--library-artists")
        .arg(fixture_path("library_artists.txt").to_str().unwrap())
        .assert()
        .success();

    // Matching releases: 1001-1003, 3001, 4001 (Radiohead=5),
    //   2001, 2002 (Joy Division=2), 6001 (Bjork=1), 9002 (Simon & Garfunkel=1)
    // Non-matching: "The Beatles" != "Beatles, The" (different normalized forms),
    //   "Various Artists" != "Various", 5001, 5002, 7001, 10001, 10002
    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 9, "Expected 9 filtered releases");
}

#[test]
fn test_limit() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(fixture_path("releases_fixture.xml").to_str().unwrap())
        .arg("--output-dir")
        .arg(dir.path().to_str().unwrap())
        .arg("--limit")
        .arg("5")
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 5, "Expected 5 releases with --limit 5");
}

#[test]
fn test_gzipped_input() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(fixture_path("releases_fixture.xml.gz").to_str().unwrap())
        .arg("--output-dir")
        .arg(dir.path().to_str().unwrap())
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 16);
}
