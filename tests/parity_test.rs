//! Cross-implementation parity for the pair-wise (artist, title) filter.
//!
//! Runs the Rust converter's `--library-db` mode against `releases_fixture.xml`
//! and runs Python `discogs-etl/scripts/filter_csv.py --library-db` against
//! the converter's *unfiltered* output, then asserts both yield the same set
//! of `release.id`s. This pins the two implementations together so the
//! converter can replace `run_pipeline.py --pair-filter` without silently
//! drifting on edge cases (diacritics, multi-artist releases, etc.).
//!
//! The test is opt-in via the `DISCOGS_ETL_REPO` env var pointing at a local
//! checkout of `WXYC/discogs-etl` with its `.venv` populated. It skips
//! gracefully when unset, so `cargo test` on a fresh laptop without the
//! sibling repo still passes.
#![allow(deprecated)] // Command::cargo_bin deprecation

use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn make_library_db(dir: &Path, rows: &[(&str, &str)]) -> PathBuf {
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

fn read_release_ids(release_csv: &Path) -> Vec<u64> {
    let mut rdr = csv::Reader::from_path(release_csv).unwrap();
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
    ids
}

#[test]
fn pair_filter_matches_python_implementation() {
    let etl_repo = match std::env::var("DISCOGS_ETL_REPO") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            eprintln!(
                "skipping parity test: set DISCOGS_ETL_REPO to a local discogs-etl \
                 checkout (with .venv populated) to run"
            );
            return;
        }
    };
    let python = etl_repo.join(".venv/bin/python");
    let filter_script = etl_repo.join("scripts/filter_csv.py");
    if !python.exists() || !filter_script.exists() {
        eprintln!(
            "skipping parity test: missing {} or {}",
            python.display(),
            filter_script.display()
        );
        return;
    }

    // Library entries chosen to exercise: exact matches, diacritic mismatches
    // both directions, multi-artist credits, and a title-only false-positive
    // candidate (Amber-by-wrong-artist) that the pair filter must drop.
    let lib_dir = tempfile::tempdir().unwrap();
    let library_db = make_library_db(
        lib_dir.path(),
        &[
            ("Autechre", "Confield"),
            ("Autechre", "Tri Repetae"),
            ("Nilufer Yanya", "PAINLESS"),
            ("Wrong Artist", "Amber"),
            ("Field, The", "From Here We Go Sublime"),
        ],
    );

    // Path A: Rust converter applies --library-db itself.
    let rust_out = tempfile::tempdir().unwrap();
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            rust_out.path().to_str().unwrap(),
            "--library-db",
            library_db.to_str().unwrap(),
        ])
        .assert()
        .success();
    let rust_ids = read_release_ids(&rust_out.path().join("release.csv"));

    // Path B: Rust converter writes unfiltered CSVs; Python filter_csv.py
    // applies --library-db over them in place.
    let py_out = tempfile::tempdir().unwrap();
    Command::cargo_bin("discogs-xml-converter")
        .unwrap()
        .args([
            "build",
            fixture_path("releases_fixture.xml").to_str().unwrap(),
            "--data-dir",
            py_out.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let py_status = std::process::Command::new(&python)
        .arg(&filter_script)
        .arg("--library-db")
        .arg(&library_db)
        .arg(py_out.path())
        .arg(py_out.path())
        .status()
        .expect("python filter_csv.py failed to launch");
    assert!(
        py_status.success(),
        "python filter_csv.py exited with non-zero status: {py_status}"
    );
    let py_ids = read_release_ids(&py_out.path().join("release.csv"));

    assert!(
        !rust_ids.is_empty(),
        "parity test is meaningless if both sides are empty"
    );
    assert_eq!(
        rust_ids, py_ids,
        "Rust pair-filter and Python filter_csv.py disagree on which releases to keep"
    );
}
