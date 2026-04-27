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
fn test_missing_artists_xml_in_directory_mode() {
    // Directory mode with releases.xml but no artists.xml -- should succeed
    // with a clear log message about missing artists, not crash
    let input_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    // Only copy releases -- deliberately omit artists.xml
    std::fs::copy(
        fixture_path("releases_fixture.xml"),
        input_dir.path().join("releases.xml"),
    )
    .unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(input_dir.path().to_str().unwrap())
        .arg("--output-dir")
        .arg(output_dir.path().to_str().unwrap())
        .assert()
        .success();

    // Releases should still be processed
    assert!(
        output_dir.path().join("release.csv").exists(),
        "release.csv should be created even without artists.xml"
    );

    let mut rdr = csv::Reader::from_path(output_dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 16, "All releases should be processed");

    // artist_alias.csv should NOT exist (no artists.xml to process)
    assert!(
        !output_dir.path().join("artist_alias.csv").exists(),
        "artist_alias.csv should not exist when artists.xml is missing"
    );
}

#[test]
fn test_release_with_many_artists_no_oom() {
    // A release with 1000+ artists should be processed without running out of memory
    let dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    // Generate an XML release with 1000+ artists
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<releases>
  <release id="99999" status="Accepted">
    <title>WXYC Compilation Vol. 1</title>
    <artists>
"#,
    );

    for i in 1..=1200 {
        xml.push_str(&format!(
            "      <artist><id>{}</id><name>Artist {}</name><anv></anv><join>,</join></artist>\n",
            i, i
        ));
    }

    xml.push_str(
        r#"    </artists>
    <labels><label catno="WXYC-001" name="WXYC Records" /></labels>
    <formats><format name="CD" qty="1" /></formats>
    <tracklist>
      <track><position>1</position><title>Track 1</title><duration>3:00</duration></track>
    </tracklist>
  </release>
</releases>"#,
    );

    let xml_path = dir.path().join("many_artists.xml");
    std::fs::write(&xml_path, xml).unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(xml_path.to_str().unwrap())
        .arg("--output-dir")
        .arg(output_dir.path().to_str().unwrap())
        .assert()
        .success();

    // Verify the release was written
    let mut rdr = csv::Reader::from_path(output_dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 1, "The many-artists release should be written");

    // Verify all 1200 artists are in release_artist.csv
    let mut rdr = csv::Reader::from_path(output_dir.path().join("release_artist.csv")).unwrap();
    let artist_count = rdr.records().count();
    assert_eq!(
        artist_count, 1200,
        "All 1200 artists should be in release_artist.csv"
    );
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
fn test_emits_json_logs_without_sentry_dsn() {
    use std::process::Command as StdCommand;

    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("discogs-xml-converter");

    let output = StdCommand::new(&bin)
        .env_remove("SENTRY_DSN")
        .arg(fixture_path("releases_fixture.xml").to_str().unwrap())
        .arg("--output-dir")
        .arg(dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "binary must run with SENTRY_DSN unset; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let has_json_line = stderr.lines().chain(stdout.lines()).any(|line| {
        line.trim_start().starts_with('{')
            && line.contains("\"level\"")
            && line.contains("\"timestamp\"")
    });
    assert!(
        has_json_line,
        "expected at least one JSON log line; stderr=\n{}\nstdout=\n{}",
        stderr, stdout
    );
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

    // Matching releases: 1001-1003, 3001, 4001 (Autechre=5),
    //   2001, 2002 (Father John Misty=2), 6001 (Nilüfer Yanya=1),
    //   9002 (Duke Ellington & John Coltrane=1)
    // Non-matching: "The Field" != "Field, The" (different normalized forms),
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

#[test]
fn test_directory_input() {
    // Create a temp directory with symlinks to fixture XML files
    let input_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    // Copy fixture files to simulate a directory of XML dumps
    std::fs::copy(
        fixture_path("releases_fixture.xml"),
        input_dir.path().join("releases.xml"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("artists_fixture.xml"),
        input_dir.path().join("artists.xml"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("labels_fixture.xml"),
        input_dir.path().join("labels.xml"),
    )
    .unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(input_dir.path().to_str().unwrap())
        .arg("--output-dir")
        .arg(output_dir.path().to_str().unwrap())
        .assert()
        .success();

    // Check release CSV files exist (same as single file mode)
    assert!(output_dir.path().join("release.csv").exists());
    assert!(output_dir.path().join("release_artist.csv").exists());

    // Check artist and label CSV files exist
    assert!(
        output_dir.path().join("artist_alias.csv").exists(),
        "artist_alias.csv should be created"
    );
    assert!(
        output_dir.path().join("artist_member.csv").exists(),
        "artist_member.csv should be created"
    );
    assert!(
        output_dir.path().join("label_hierarchy.csv").exists(),
        "label_hierarchy.csv should be created"
    );

    // Check content of label_hierarchy.csv
    let mut rdr = csv::Reader::from_path(output_dir.path().join("label_hierarchy.csv")).unwrap();
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    // 4 labels have parents: Parlophone->EMI, Capitol Records->EMI,
    // Matador Records->Beggars Group, 4AD->Beggars Group
    assert_eq!(records.len(), 4, "Expected 4 label hierarchy entries");

    // Check releases were processed
    let mut rdr = csv::Reader::from_path(output_dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 16, "Expected 16 releases (no filter)");
}

#[test]
fn test_directory_input_with_alias_filtering() {
    // Test that alias-enhanced filtering works end-to-end.
    // The releases_fixture.xml has a release credited to "Field, The" (id 8).
    // The library_artists.txt has "The Field".
    // Without aliases, "Field, The" != "The Field" (no match).
    // We'll create an artists.xml that maps artist_id 8 to alias "The Field".
    let input_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    // Copy releases fixture
    std::fs::copy(
        fixture_path("releases_fixture.xml"),
        input_dir.path().join("releases.xml"),
    )
    .unwrap();

    // Create a custom artists.xml that maps artist_id 8 to alias "The Field"
    let artists_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<artists>
  <artist>
    <id>8</id>
    <name>Field, The</name>
    <namevariations>
      <name>The Field</name>
    </namevariations>
  </artist>
</artists>
"#;
    std::fs::write(input_dir.path().join("artists.xml"), artists_xml).unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg(input_dir.path().to_str().unwrap())
        .arg("--output-dir")
        .arg(output_dir.path().to_str().unwrap())
        .arg("--library-artists")
        .arg(fixture_path("library_artists.txt").to_str().unwrap())
        .assert()
        .success();

    // Without alias: 9 matches. With alias: "Field, The" now matches via
    // alias "The Field" -> should be 10.
    let mut rdr = csv::Reader::from_path(output_dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(
        count, 10,
        "Expected 10 filtered releases (9 canonical + 1 via alias)"
    );
}

#[test]
fn test_single_file_backward_compatible() {
    // Verify single file mode still works identically
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

    // Same result as before: 9 filtered releases
    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(
        count, 9,
        "Single file mode should produce same results as before"
    );

    // artist_alias.csv should NOT exist in single file mode
    assert!(
        !dir.path().join("artist_alias.csv").exists(),
        "artist_alias.csv should not be created in single file mode"
    );
}
