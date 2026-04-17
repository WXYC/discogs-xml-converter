//! PG direct-import integration test for PgOutput mode.
//!
//! Parses releases_fixture.xml using the PgOutput backend, then verifies
//! the database state matches the CSV oracle expectations. Tests both
//! unfiltered and filtered (library_artists.txt) import modes.
//!
//! Gated on TEST_DATABASE_URL. Skips when the env var is unset.

use std::path::PathBuf;

use discogs_xml_converter::output::ReleaseOutput;
use discogs_xml_converter::parser::parse_releases;
use discogs_xml_converter::pg_output::PgOutput;

fn test_db_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Create a temporary database for test isolation.
struct TempDb {
    admin_url: String,
    db_name: String,
    pub url: String,
}

impl TempDb {
    fn new(admin_url: &str) -> Self {
        let db_name = format!("discogs_pg_test_{:08x}", rand_u32());
        let mut client = postgres::Client::connect(admin_url, postgres::NoTls).unwrap();
        client
            .execute(&format!("CREATE DATABASE {}", db_name), &[])
            .unwrap();
        let base = admin_url.rsplit_once('/').unwrap().0;
        let url = format!("{}/{}", base, db_name);
        Self {
            admin_url: admin_url.to_string(),
            db_name,
            url,
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        if let Ok(mut client) = postgres::Client::connect(&self.admin_url, postgres::NoTls) {
            let _ = client.execute(
                &format!(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}' AND pid <> pg_backend_pid()",
                    self.db_name
                ),
                &[],
            );
            let _ = client.execute(
                &format!("DROP DATABASE IF EXISTS {}", self.db_name),
                &[],
            );
        }
    }
}

fn rand_u32() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::SystemTime;
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    (d.subsec_nanos() ^ (d.as_secs() as u32) ^ seq).wrapping_mul(2654435761)
}

/// Set up the discogs-cache schema in a test database.
fn set_up_schema(client: &mut postgres::Client) {
    client
        .batch_execute(
            "DROP TABLE IF EXISTS cache_metadata CASCADE;
             DROP TABLE IF EXISTS release_track_count CASCADE;
             DROP TABLE IF EXISTS release_track_artist CASCADE;
             DROP TABLE IF EXISTS release_track CASCADE;
             DROP TABLE IF EXISTS release_label CASCADE;
             DROP TABLE IF EXISTS release_artist CASCADE;
             DROP TABLE IF EXISTS release_genre CASCADE;
             DROP TABLE IF EXISTS release_style CASCADE;
             DROP TABLE IF EXISTS release_company CASCADE;
             DROP TABLE IF EXISTS release CASCADE;

             CREATE TABLE release (
                 id integer PRIMARY KEY,
                 title text NOT NULL,
                 release_year smallint,
                 country text,
                 artwork_url text,
                 master_id integer
             );
             CREATE TABLE release_artist (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 artist_id integer,
                 artist_name text NOT NULL,
                 extra integer DEFAULT 0
             );
             CREATE TABLE release_label (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 label_name text NOT NULL,
                 catno text
             );
             CREATE TABLE release_track (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 sequence integer NOT NULL,
                 position text,
                 title text NOT NULL,
                 duration text
             );
             CREATE TABLE release_track_artist (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 track_sequence integer NOT NULL,
                 artist_name text NOT NULL
             );
             CREATE TABLE release_genre (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 genre text NOT NULL
             );
             CREATE TABLE release_style (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 style text NOT NULL
             );
             CREATE TABLE release_company (
                 release_id integer NOT NULL REFERENCES release(id) ON DELETE CASCADE,
                 company_id integer,
                 company_name text NOT NULL,
                 entity_type integer,
                 entity_type_name text NOT NULL
             );
             CREATE TABLE cache_metadata (
                 release_id integer PRIMARY KEY REFERENCES release(id) ON DELETE CASCADE,
                 cached_at timestamptz NOT NULL DEFAULT now(),
                 source text NOT NULL,
                 last_validated timestamptz
             );",
        )
        .unwrap();
}

/// Parse the releases fixture through PgOutput.
fn import_fixture_releases(db_url: &str) {
    let xml_path = fixture_path("releases_fixture.xml");
    let mut pg_output = PgOutput::new(db_url, 100_000).unwrap();

    parse_releases(&xml_path, None, 100_000, |release| {
        pg_output.write_release(&release).unwrap();
    })
    .unwrap();

    pg_output.finish().unwrap();
}

// -- Row counts from the CSV oracle --
// release.csv has 16 data rows, but one (id=7001) has empty title and is skipped by PgOutput.
// That leaves 15 releases.
const EXPECTED_RELEASE_COUNT: i64 = 15;
const EXPECTED_TRACK_COUNT: i64 = 30; // from release_track.csv (31 lines = 1 header + 30 data rows)

#[test]
fn test_pg_output_full_pipeline() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // Verify release count
    let row = client
        .query_one("SELECT COUNT(*) FROM release", &[])
        .unwrap();
    let release_count: i64 = row.get(0);
    assert_eq!(
        release_count, EXPECTED_RELEASE_COUNT,
        "Release count should match CSV oracle (minus empty-title release 7001)"
    );

    // Verify track count matches CSV oracle
    let row = client
        .query_one("SELECT COUNT(*) FROM release_track", &[])
        .unwrap();
    let track_count: i64 = row.get(0);
    assert_eq!(
        track_count, EXPECTED_TRACK_COUNT,
        "Track count should match CSV oracle"
    );

    // Verify release_artist is populated
    let row = client
        .query_one("SELECT COUNT(*) FROM release_artist", &[])
        .unwrap();
    let artist_count: i64 = row.get(0);
    assert!(
        artist_count >= EXPECTED_RELEASE_COUNT,
        "Each release should have at least one artist"
    );

    // Verify release_label is populated
    let row = client
        .query_one("SELECT COUNT(*) FROM release_label", &[])
        .unwrap();
    let label_count: i64 = row.get(0);
    assert!(label_count > 0, "release_label should have rows");

    // Verify release_genre is populated (release 1001 has Electronic, Rock)
    let row = client
        .query_one("SELECT COUNT(*) FROM release_genre", &[])
        .unwrap();
    let genre_count: i64 = row.get(0);
    assert!(genre_count > 0, "release_genre should have rows");

    // Verify release_style is populated (release 1001 has Alternative Rock, Art Rock)
    let row = client
        .query_one("SELECT COUNT(*) FROM release_style", &[])
        .unwrap();
    let style_count: i64 = row.get(0);
    assert!(style_count > 0, "release_style should have rows");

    // Verify release_company is populated
    let row = client
        .query_one("SELECT COUNT(*) FROM release_company", &[])
        .unwrap();
    let company_count: i64 = row.get(0);
    assert!(company_count > 0, "release_company should have rows");

    // Verify track artists exist (release 8001 has track-level artists)
    let row = client
        .query_one("SELECT COUNT(*) FROM release_track_artist", &[])
        .unwrap();
    let track_artist_count: i64 = row.get(0);
    assert!(
        track_artist_count >= 2,
        "release_track_artist should have track artists from compilation"
    );
}

#[test]
fn test_pg_output_artwork_urls() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // Release 1001 has a primary image
    let row = client
        .query_one(
            "SELECT artwork_url FROM release WHERE id = 1001",
            &[],
        )
        .unwrap();
    let url: Option<&str> = row.get(0);
    assert!(
        url.is_some(),
        "Release 1001 should have artwork_url (has primary image)"
    );
    assert!(
        url.unwrap().contains("release-1001"),
        "Artwork URL should reference release 1001"
    );

    // Release 3001 has a primary image
    let row = client
        .query_one(
            "SELECT artwork_url FROM release WHERE id = 3001",
            &[],
        )
        .unwrap();
    let url: Option<&str> = row.get(0);
    assert!(
        url.is_some(),
        "Release 3001 should have artwork_url (has primary image)"
    );

    // Release 2001 has only a secondary image; PgOutput should still pick it
    let row = client
        .query_one(
            "SELECT artwork_url FROM release WHERE id = 2001",
            &[],
        )
        .unwrap();
    let url: Option<&str> = row.get(0);
    assert!(
        url.is_some(),
        "Release 2001 should have artwork_url (secondary image fallback)"
    );

    // Release 1002 has no images
    let row = client
        .query_one(
            "SELECT artwork_url FROM release WHERE id = 1002",
            &[],
        )
        .unwrap();
    let url: Option<&str> = row.get(0);
    assert!(
        url.is_none(),
        "Release 1002 should have no artwork_url (no images)"
    );
}

#[test]
fn test_pg_output_cache_metadata() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // Every release should have a cache_metadata entry
    let row = client
        .query_one("SELECT COUNT(*) FROM cache_metadata", &[])
        .unwrap();
    let meta_count: i64 = row.get(0);
    assert_eq!(
        meta_count, EXPECTED_RELEASE_COUNT,
        "Every release should have cache_metadata"
    );

    // Source should be 'bulk_import'
    let row = client
        .query_one(
            "SELECT DISTINCT source FROM cache_metadata",
            &[],
        )
        .unwrap();
    let source: &str = row.get(0);
    assert_eq!(source, "bulk_import");
}

#[test]
fn test_pg_output_release_track_count() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // release_track_count should exist and be populated
    let row = client
        .query_one("SELECT COUNT(*) FROM release_track_count", &[])
        .unwrap();
    let count: i64 = row.get(0);
    assert!(
        count > 0,
        "release_track_count should be populated after finish()"
    );

    // Release 1001 (OK Computer UK CD) has 3 tracks
    let row = client
        .query_one(
            "SELECT track_count FROM release_track_count WHERE release_id = 1001",
            &[],
        )
        .unwrap();
    let track_count: i32 = row.get(0);
    assert_eq!(track_count, 3, "Release 1001 should have 3 tracks");

    // Release 1002 (OK Computer US Vinyl) has 5 tracks
    let row = client
        .query_one(
            "SELECT track_count FROM release_track_count WHERE release_id = 1002",
            &[],
        )
        .unwrap();
    let track_count: i32 = row.get(0);
    assert_eq!(track_count, 5, "Release 1002 should have 5 tracks");
}

#[test]
fn test_pg_output_specific_release_data() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // Release 1001: OK Computer UK
    let row = client
        .query_one(
            "SELECT title, release_year, country, master_id FROM release WHERE id = 1001",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, &str>(0), "OK Computer");
    assert_eq!(row.get::<_, Option<i16>>(1), Some(1997));
    assert_eq!(row.get::<_, Option<&str>>(2), Some("UK"));
    assert_eq!(row.get::<_, Option<i32>>(3), Some(500));

    // Release 6001: Homogenic (bad date "Unknown" -> NULL year)
    let row = client
        .query_one(
            "SELECT title, release_year FROM release WHERE id = 6001",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, &str>(0), "Homogenic");
    assert_eq!(
        row.get::<_, Option<i16>>(1),
        None,
        "Bad date 'Unknown' should produce NULL year"
    );

    // Release 4001: Amnesiac (no master_id)
    let row = client
        .query_one(
            "SELECT master_id FROM release WHERE id = 4001",
            &[],
        )
        .unwrap();
    assert_eq!(
        row.get::<_, Option<i32>>(0),
        None,
        "Amnesiac should have no master_id"
    );

    // Release 9002: Simon & Garfunkel (ampersand in name)
    let rows = client
        .query(
            "SELECT artist_name FROM release_artist WHERE release_id = 9002",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, &str>(0),
        "Simon & Garfunkel",
        "Ampersand should be preserved in artist name"
    );

    // Release 5002: empty released date -> NULL year
    let row = client
        .query_one(
            "SELECT release_year FROM release WHERE id = 5002",
            &[],
        )
        .unwrap();
    assert_eq!(
        row.get::<_, Option<i16>>(0),
        None,
        "Empty released date should produce NULL year"
    );
}

#[test]
fn test_pg_output_empty_title_skipped() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // Release 7001 has empty title and should be skipped
    let rows = client
        .query("SELECT id FROM release WHERE id = 7001", &[])
        .unwrap();
    assert!(
        rows.is_empty(),
        "Release 7001 (empty title) should be skipped by PgOutput"
    );
}

#[test]
fn test_pg_output_referential_integrity() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    import_fixture_releases(&temp_db.url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // All child tables should reference valid release IDs
    let child_tables = [
        "release_artist",
        "release_label",
        "release_track",
        "release_track_artist",
        "release_genre",
        "release_style",
        "release_company",
        "cache_metadata",
    ];

    for table in &child_tables {
        let query = format!(
            "SELECT COUNT(*) FROM {} c LEFT JOIN release r ON c.release_id = r.id WHERE r.id IS NULL",
            table
        );
        let row = client.query_one(&query, &[]).unwrap();
        let orphan_count: i64 = row.get(0);
        assert_eq!(
            orphan_count, 0,
            "Table {} should have no orphan release_id references",
            table
        );
    }
}

#[test]
fn test_pg_output_with_library_filter() {
    let Some(admin_url) = test_db_url() else {
        return;
    };
    let temp_db = TempDb::new(&admin_url);

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();
    set_up_schema(&mut client);
    drop(client);

    // Parse with library_artists filter
    let xml_path = fixture_path("releases_fixture.xml");
    let filter_path = fixture_path("library_artists.txt");
    let filter =
        discogs_xml_converter::filter::ArtistFilter::from_file(&filter_path).unwrap();

    let mut pg_output = PgOutput::new(&temp_db.url, 100_000).unwrap();

    parse_releases(&xml_path, None, 100_000, |release| {
        // Only write releases that match the artist filter
        let names: Vec<&str> = release.artists.iter().map(|a| a.name.as_str()).collect();
        if filter.matches_any(names.iter().copied()) {
            pg_output.write_release(&release).unwrap();
        }
    })
    .unwrap();

    pg_output.finish().unwrap();

    let mut client = postgres::Client::connect(&temp_db.url, postgres::NoTls).unwrap();

    // With the library_artists.txt filter, only these artists should match:
    // Radiohead (1001, 1002, 1003, 3001, 4001), Joy Division (2001, 2002),
    // The Beatles (9001), Simon & Garfunkel (9002), Bjork (6001), Various (8001)
    let row = client
        .query_one("SELECT COUNT(*) FROM release", &[])
        .unwrap();
    let release_count: i64 = row.get(0);

    // Releases NOT matching: 5001 (DJ Unknown), 5002 (Mystery Band), 10001 (Random Artist X),
    // 10002 (Obscure Band Y), 7001 (Empty Title Artist - also empty title)
    // So: 15 total valid - 4 non-matching = 11 filtered releases
    // (7001 is also empty title so wouldn't show up either way)
    assert!(
        release_count > 0 && release_count < EXPECTED_RELEASE_COUNT,
        "Filtered import should have fewer releases ({}) than unfiltered ({})",
        release_count,
        EXPECTED_RELEASE_COUNT
    );

    // Verify DJ Unknown is not in the database
    let rows = client
        .query(
            "SELECT artist_name FROM release_artist WHERE artist_name = 'DJ Unknown'",
            &[],
        )
        .unwrap();
    assert!(
        rows.is_empty(),
        "DJ Unknown should not appear in filtered import"
    );

    // Verify Radiohead IS in the database
    let rows = client
        .query(
            "SELECT DISTINCT artist_name FROM release_artist WHERE artist_name = 'Radiohead'",
            &[],
        )
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "Radiohead should appear in filtered import"
    );
}
