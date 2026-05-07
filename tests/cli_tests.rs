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
        .arg("build")
        .arg(input_dir.path().to_str().unwrap())
        .arg("--data-dir")
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
        .arg("build")
        .arg(xml_path.to_str().unwrap())
        .arg("--data-dir")
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
fn test_help_lists_subcommands() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("build"))
        .stdout(predicate::str::contains("import"));
}

#[test]
fn test_emits_json_logs_without_sentry_dsn() {
    use std::process::Command as StdCommand;

    let dir = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("discogs-xml-converter");

    let output = StdCommand::new(&bin)
        .env_remove("SENTRY_DSN")
        .arg("build")
        .arg(fixture_path("releases_fixture.xml").to_str().unwrap())
        .arg("--data-dir")
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
fn test_build_help_shows_data_dir_and_resume() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args(["build", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--data-dir"))
        .stdout(predicate::str::contains("--state-file"))
        .stdout(predicate::str::contains("--resume"));
}

#[test]
fn test_import_help_shows_database_url_and_fresh() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args(["import", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--database-url"))
        .stdout(predicate::str::contains("--fresh"));
}

#[test]
fn test_no_subcommand_fails() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .assert()
        .failure();
}

#[test]
fn test_build_requires_input() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .arg("build")
        .assert()
        .failure();
}

#[test]
fn test_import_without_database_url_or_env_fails() {
    // Clear the env var so the fallback doesn't kick in.
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .env_remove("DATABASE_URL_DISCOGS")
        .args([
            "import",
            fixture_path("single_release.xml").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("DATABASE_URL_DISCOGS"));
}

#[test]
fn test_import_uses_database_url_env_fallback() {
    // The env var resolves but the URL is unreachable; the failure must come
    // from the connection attempt, not from the missing-flag path.
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .env("DATABASE_URL_DISCOGS", "postgresql://127.0.0.1:1/never")
        .args([
            "import",
            fixture_path("single_release.xml").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("DATABASE_URL_DISCOGS").not());
}

#[test]
fn test_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
        ])
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

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    assert_eq!(rdr.records().count(), 16);
}

#[test]
fn test_with_library_artists_filter() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--library-artists",
            fixture_path("library_artists.txt").to_str().unwrap(),
        ])
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
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--limit",
            "5",
        ])
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 5, "Expected 5 releases with --limit 5");
}

/// Build a small `library.db` with the supplied `(artist, title)` rows for
/// pair-filter integration tests. Returns the path to the file rooted in
/// `dir`. Uses the canonical schema produced by `wxyc-export-to-sqlite`.
fn make_test_library_db(dir: &std::path::Path, rows: &[(&str, &str)]) -> PathBuf {
    let path = dir.join("library.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE library (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            artist TEXT NOT NULL,\
            title TEXT NOT NULL,\
            format TEXT\
         );",
    )
    .unwrap();
    {
        let mut stmt = conn
            .prepare("INSERT INTO library (artist, title) VALUES (?1, ?2)")
            .unwrap();
        for (artist, title) in rows {
            stmt.execute(rusqlite::params![artist, title]).unwrap();
        }
    }
    drop(conn);
    path
}

#[test]
fn test_with_library_db_pair_filter() {
    // Exercises the new `--library-db` mode end-to-end through the CLI.
    // The library.db keeps only:
    //   - (Autechre, Confield)        -> matches releases 1001, 1002, 1003
    //   - (Nilüfer Yanya, PAINLESS)   -> matches release 6001 (diacritic case)
    //   - (Wrong Artist, Amber)       -> excludes release 3001 (title in DB
    //                                    but artist on the release doesn't
    //                                    match the library set for that title)
    //   - (Autechre, Tri Repetae) is intentionally OMITTED so release 4001
    //     is excluded even though its artist appears in the library.
    let dir = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let library_db = make_test_library_db(
        lib.path(),
        &[
            ("Autechre", "Confield"),
            ("Nilüfer Yanya", "PAINLESS"),
            ("Wrong Artist", "Amber"),
        ],
    );

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--library-db",
            library_db.to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let id_idx = rdr
        .headers()
        .unwrap()
        .iter()
        .position(|h| h == "id")
        .unwrap();
    let mut ids: Vec<u64> = rdr
        .records()
        .map(|r| r.unwrap()[id_idx].parse().unwrap())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![1001, 1002, 1003, 6001],
        "pair-filter should keep exactly the 4 releases whose (artist, title) appears in library.db"
    );
}

#[test]
fn test_library_db_and_library_artists_are_mutually_exclusive() {
    // clap should reject a combo of both flags before any work begins.
    let dir = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let library_db = make_test_library_db(lib.path(), &[("Autechre", "Confield")]);

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--library-artists",
            fixture_path("library_artists.txt").to_str().unwrap(),
            "--library-db",
            library_db.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("library-artists").or(predicate::str::contains("library-db")),
        );
}

#[test]
fn test_library_db_missing_file_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--library-db",
            "/nonexistent/library.db",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("library.db"));
}

#[test]
fn test_xml_type_flag_processes_as_releases() {
    // --xml-type=releases lets the caller bypass the per-file root-element
    // detection. This matters when the input is a stream-only source (named
    // pipe / process substitution): detect_xml_type opens the file, reads
    // the root element, and closes. On a FIFO, that close kills any upstream
    // writer (e.g. a backgrounded `curl -o FIFO`) with SIGPIPE before the
    // real scan ever opens the file. With --xml-type, the detection open is
    // skipped and the FIFO is opened exactly once.
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--xml-type",
            "releases",
        ])
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 16, "release.csv should have all 16 fixture releases");
}

#[test]
fn test_xml_type_flag_rejects_invalid_value() {
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--xml-type",
            "nonsense",
        ])
        .assert()
        .failure();
}

#[test]
fn test_gzipped_input() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml.gz").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 16);
}

#[test]
fn test_directory_input() {
    let input_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();

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
        .args([
            "build",
            input_dir.path().to_str().unwrap(),
            "--data-dir",
            output_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output_dir.path().join("release.csv").exists());
    assert!(output_dir.path().join("release_artist.csv").exists());
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

    let mut rdr = csv::Reader::from_path(output_dir.path().join("label_hierarchy.csv")).unwrap();
    let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
    assert_eq!(records.len(), 4, "Expected 4 label hierarchy entries");

    let mut rdr = csv::Reader::from_path(output_dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(count, 16, "Expected 16 releases (no filter)");
}

#[test]
fn test_directory_input_with_alias_filtering() {
    let input_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();

    std::fs::copy(
        fixture_path("releases_fixture.xml"),
        input_dir.path().join("releases.xml"),
    )
    .unwrap();

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
        .args([
            "build",
            input_dir.path().to_str().unwrap(),
            "--data-dir",
            output_dir.path().to_str().unwrap(),
            "--library-artists",
            fixture_path("library_artists.txt").to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(output_dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(
        count, 10,
        "Expected 10 filtered releases (9 canonical + 1 via alias)"
    );
}

#[test]
fn test_single_file_build_mode() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--library-artists",
            fixture_path("library_artists.txt").to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    let count = rdr.records().count();
    assert_eq!(
        count, 9,
        "Single-file mode should produce same results as before"
    );

    assert!(
        !dir.path().join("artist_alias.csv").exists(),
        "artist_alias.csv should not be created in single file mode"
    );
}

#[test]
fn test_deprecated_output_dir_alias_still_works() {
    let dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--output-dir",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("--output-dir is deprecated"));

    let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
    assert_eq!(rdr.records().count(), 16);
}
