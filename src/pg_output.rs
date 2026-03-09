//! PostgreSQL direct output for release data.
//!
//! Provides `PgOutput`, which implements `ReleaseOutput` to stream releases
//! directly into PostgreSQL via COPY, eliminating the CSV round-trip.
//!
//! Also contains pure transform functions that replicate the logic from
//! `discogs-cache/scripts/import_csv.py`.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::{Context, Result};
use log::info;

use crate::model::Release;
use crate::output::ReleaseOutput;

/// Extract a 4-digit year from a Discogs "released" field.
///
/// Matches the behavior of `import_csv.py:extract_year()`.
pub fn extract_year(released: &str) -> Option<i16> {
    if released.len() >= 4 && released.as_bytes()[..4].iter().all(|b| b.is_ascii_digit()) {
        released[..4].parse().ok()
    } else {
        None
    }
}

/// Convert an empty string to None.
pub fn empty_to_none(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Escape a string for PostgreSQL COPY TEXT format.
///
/// COPY TEXT uses tab-delimited rows with backslash escaping:
/// - `\` → `\\`
/// - newline → `\n`
/// - carriage return → `\r`
/// - tab → `\t`
pub fn escape_copy_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Format a value for a COPY TEXT column.
///
/// None values become `\N` (PostgreSQL NULL). Non-empty strings are escaped.
fn copy_value(val: Option<&str>) -> String {
    match val {
        None => "\\N".to_string(),
        Some("") => "\\N".to_string(),
        Some(s) => escape_copy_text(s),
    }
}

/// Format a COPY TEXT row from a slice of column values.
///
/// Joins values with tabs and appends a newline.
pub fn copy_line(values: &[Option<&str>]) -> String {
    let mut line = String::new();
    for (i, val) in values.iter().enumerate() {
        if i > 0 {
            line.push('\t');
        }
        line.push_str(&copy_value(*val));
    }
    line.push('\n');
    line
}

/// Pick the best artwork URL from a release's images.
///
/// Prefers the first "primary" image; falls back to the first image of any type.
pub fn pick_artwork_url(images: &[crate::model::ReleaseImage]) -> Option<&str> {
    let primary = images
        .iter()
        .find(|img| img.image_type == "primary")
        .map(|img| img.uri.as_str());
    primary.or_else(|| images.first().map(|img| img.uri.as_str()))
}

/// Dedup tracker for (release_id, artist_name) pairs in release_artist table.
#[derive(Default)]
pub struct ArtistDedup {
    seen: HashSet<(u64, String)>,
}

impl ArtistDedup {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    /// Returns true if this is the first occurrence (not a duplicate).
    pub fn insert(&mut self, release_id: u64, artist_name: &str) -> bool {
        self.seen.insert((release_id, artist_name.to_string()))
    }
}

/// Dedup tracker for (release_id, track_sequence, artist_name) in release_track_artist.
#[derive(Default)]
pub struct TrackArtistDedup {
    seen: HashSet<(u64, u32, String)>,
}

impl TrackArtistDedup {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    /// Returns true if this is the first occurrence (not a duplicate).
    pub fn insert(&mut self, release_id: u64, track_seq: u32, artist_name: &str) -> bool {
        self.seen
            .insert((release_id, track_seq, artist_name.to_string()))
    }
}

/// Dedup tracker for (release_id, label_name) pairs in release_label table.
#[derive(Default)]
pub struct LabelDedup {
    seen: HashSet<(u64, String)>,
}

impl LabelDedup {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    /// Returns true if this is the first occurrence (not a duplicate).
    pub fn insert(&mut self, release_id: u64, label_name: &str) -> bool {
        self.seen.insert((release_id, label_name.to_string()))
    }
}

/// Accumulated artwork URLs for post-import UPDATE.
pub type ArtworkMap = HashMap<u64, String>;

/// Accumulated track counts for release_track_count table.
pub type TrackCountMap = HashMap<u64, u32>;

/// Streams release data directly into PostgreSQL via COPY.
///
/// Buffers releases in memory and periodically flushes to PostgreSQL.
/// Within each flush, tables are written in FK order: `release` first,
/// then child tables.
pub struct PgOutput {
    client: postgres::Client,
    buf_release: Vec<u8>,
    buf_release_artist: Vec<u8>,
    buf_release_label: Vec<u8>,
    buf_release_track: Vec<u8>,
    buf_release_track_artist: Vec<u8>,
    artist_dedup: ArtistDedup,
    label_dedup: LabelDedup,
    track_artist_dedup: TrackArtistDedup,
    artwork: ArtworkMap,
    track_counts: TrackCountMap,
    batch_count: usize,
    batch_size: usize,
    total_written: usize,
}

impl PgOutput {
    /// Connect to PostgreSQL and prepare for streaming release data.
    pub fn new(database_url: &str, batch_size: usize) -> Result<Self> {
        let client = postgres::Client::connect(database_url, postgres::NoTls)
            .with_context(|| format!("Failed to connect to PostgreSQL at {}", database_url))?;
        Ok(Self {
            client,
            buf_release: Vec::new(),
            buf_release_artist: Vec::new(),
            buf_release_label: Vec::new(),
            buf_release_track: Vec::new(),
            buf_release_track_artist: Vec::new(),
            artist_dedup: ArtistDedup::new(),
            label_dedup: LabelDedup::new(),
            track_artist_dedup: TrackArtistDedup::new(),
            artwork: HashMap::new(),
            track_counts: HashMap::new(),
            batch_count: 0,
            batch_size,
            total_written: 0,
        })
    }

    /// Number of releases flushed to PostgreSQL so far.
    pub fn total_written(&self) -> usize {
        self.total_written
    }

    /// COPY a buffer of COPY TEXT data into a table. No-op if buffer is empty.
    fn copy_buffer(client: &mut postgres::Client, stmt: &str, buf: &[u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut writer = client.copy_in(stmt)?;
        writer.write_all(buf)?;
        writer.finish()?;
        Ok(())
    }
}

impl ReleaseOutput for PgOutput {
    fn write_release(&mut self, release: &Release) -> Result<()> {
        // Skip releases with empty title (required field)
        if release.title.is_empty() {
            return Ok(());
        }

        let id_str = release.id.to_string();
        let year_str = extract_year(&release.released).map(|y| y.to_string());
        let master_id_str = release.master_id.map(|id| id.to_string());

        // release row: id, title, release_year, country, master_id
        let line = copy_line(&[
            Some(&id_str),
            Some(&release.title),
            year_str.as_deref(),
            empty_to_none(&release.country),
            master_id_str.as_deref(),
        ]);
        self.buf_release.extend_from_slice(line.as_bytes());

        // release_artist rows (main artists, extra=0)
        for artist in &release.artists {
            if artist.name.is_empty() {
                continue;
            }
            if !self.artist_dedup.insert(release.id, &artist.name) {
                continue;
            }
            let artist_id_str = artist.artist_id.to_string();
            let line = copy_line(&[
                Some(&id_str),
                Some(&artist_id_str),
                Some(&artist.name),
                Some("0"),
            ]);
            self.buf_release_artist.extend_from_slice(line.as_bytes());
        }

        // release_artist rows (extra artists, extra=1)
        for artist in &release.extra_artists {
            if artist.name.is_empty() {
                continue;
            }
            if !self.artist_dedup.insert(release.id, &artist.name) {
                continue;
            }
            let artist_id_str = artist.artist_id.to_string();
            let line = copy_line(&[
                Some(&id_str),
                Some(&artist_id_str),
                Some(&artist.name),
                Some("1"),
            ]);
            self.buf_release_artist.extend_from_slice(line.as_bytes());
        }

        // release_label rows (release_id, label_name only -- catno is not in the DB)
        for label in &release.labels {
            if label.name.is_empty() {
                continue;
            }
            if !self.label_dedup.insert(release.id, &label.name) {
                continue;
            }
            let line = copy_line(&[Some(&id_str), Some(&label.name)]);
            self.buf_release_label.extend_from_slice(line.as_bytes());
        }

        // release_track rows + track count
        let track_count = release.tracks.len() as u32;
        if track_count > 0 {
            self.track_counts.insert(release.id, track_count);
        }

        for (idx, track) in release.tracks.iter().enumerate() {
            // Skip tracks with empty title (required field)
            if track.title.is_empty() {
                continue;
            }
            let seq = (idx + 1) as u32;
            let seq_str = seq.to_string();
            let line = copy_line(&[
                Some(&id_str),
                Some(&seq_str),
                empty_to_none(&track.position),
                Some(&track.title),
                empty_to_none(&track.duration),
            ]);
            self.buf_release_track.extend_from_slice(line.as_bytes());

            // Track artists (both main and extra)
            for artist in track.artists.iter().chain(track.extra_artists.iter()) {
                if !self
                    .track_artist_dedup
                    .insert(release.id, seq, &artist.name)
                {
                    continue;
                }
                let line = copy_line(&[Some(&id_str), Some(&seq_str), Some(&artist.name)]);
                self.buf_release_track_artist
                    .extend_from_slice(line.as_bytes());
            }
        }

        // Artwork (accumulated for batch UPDATE in finish())
        if let Some(url) = pick_artwork_url(&release.images) {
            self.artwork.insert(release.id, url.to_string());
        }

        // Flush if batch is full
        self.batch_count += 1;
        if self.batch_count >= self.batch_size {
            self.flush()?;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.batch_count == 0 {
            return Ok(());
        }

        // FK-safe ordering: release (parent) first, then child tables
        Self::copy_buffer(
            &mut self.client,
            "COPY release (id, title, release_year, country, master_id) FROM STDIN",
            &self.buf_release,
        )?;
        Self::copy_buffer(
            &mut self.client,
            "COPY release_artist (release_id, artist_id, artist_name, extra) FROM STDIN",
            &self.buf_release_artist,
        )?;
        Self::copy_buffer(
            &mut self.client,
            "COPY release_label (release_id, label_name) FROM STDIN",
            &self.buf_release_label,
        )?;
        Self::copy_buffer(
            &mut self.client,
            "COPY release_track (release_id, sequence, position, title, duration) FROM STDIN",
            &self.buf_release_track,
        )?;
        Self::copy_buffer(
            &mut self.client,
            "COPY release_track_artist (release_id, track_sequence, artist_name) FROM STDIN",
            &self.buf_release_track_artist,
        )?;

        self.total_written += self.batch_count;
        info!(
            "Flushed {} releases to PostgreSQL ({} total)",
            self.batch_count, self.total_written
        );

        self.buf_release.clear();
        self.buf_release_artist.clear();
        self.buf_release_label.clear();
        self.buf_release_track.clear();
        self.buf_release_track_artist.clear();
        self.batch_count = 0;

        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        // 1. Flush remaining buffered rows
        PgOutput::flush(self)?;

        // 2. Artwork URL population via temp table + UPDATE JOIN
        if !self.artwork.is_empty() {
            info!(
                "Updating {} releases with artwork URLs...",
                self.artwork.len()
            );
            self.client.execute(
                "CREATE TEMP TABLE _artwork (release_id integer PRIMARY KEY, artwork_url text NOT NULL)",
                &[],
            )?;

            {
                let mut writer = self
                    .client
                    .copy_in("COPY _artwork (release_id, artwork_url) FROM STDIN")?;
                for (release_id, url) in &self.artwork {
                    let line = copy_line(&[Some(&release_id.to_string()), Some(url)]);
                    writer.write_all(line.as_bytes())?;
                }
                writer.finish()?;
            }

            self.client.execute(
                "UPDATE release r SET artwork_url = a.artwork_url FROM _artwork a WHERE r.id = a.release_id",
                &[],
            )?;
            self.client.execute("DROP TABLE _artwork", &[])?;
        }

        // 3. Track count table
        self.client
            .execute("DROP TABLE IF EXISTS release_track_count", &[])?;
        self.client.execute(
            "CREATE UNLOGGED TABLE release_track_count (release_id integer PRIMARY KEY, track_count integer NOT NULL)",
            &[],
        )?;
        if !self.track_counts.is_empty() {
            info!(
                "Creating release_track_count with {} entries...",
                self.track_counts.len()
            );
            let mut writer = self
                .client
                .copy_in("COPY release_track_count (release_id, track_count) FROM STDIN")?;
            for (release_id, count) in &self.track_counts {
                let line = copy_line(&[Some(&release_id.to_string()), Some(&count.to_string())]);
                writer.write_all(line.as_bytes())?;
            }
            writer.finish()?;
        }

        // 4. Cache metadata
        self.client.execute(
            "INSERT INTO cache_metadata (release_id, source) SELECT id, 'bulk_import' FROM release ON CONFLICT (release_id) DO NOTHING",
            &[],
        )?;

        info!("PostgreSQL import complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReleaseImage;

    // -- extract_year tests --

    #[test]
    fn test_extract_year_full_date() {
        assert_eq!(extract_year("1997-06-16"), Some(1997));
    }

    #[test]
    fn test_extract_year_year_only() {
        assert_eq!(extract_year("1997"), Some(1997));
    }

    #[test]
    fn test_extract_year_malformed() {
        assert_eq!(extract_year("Unknown"), None);
    }

    #[test]
    fn test_extract_year_empty() {
        assert_eq!(extract_year(""), None);
    }

    #[test]
    fn test_extract_year_partial_digits() {
        assert_eq!(extract_year("199"), None);
    }

    #[test]
    fn test_extract_year_leading_zeros() {
        assert_eq!(extract_year("0001-01-01"), Some(1));
    }

    // -- empty_to_none tests --

    #[test]
    fn test_empty_to_none_empty() {
        assert_eq!(empty_to_none(""), None);
    }

    #[test]
    fn test_empty_to_none_non_empty() {
        assert_eq!(empty_to_none("hello"), Some("hello"));
    }

    // -- escape_copy_text tests --

    #[test]
    fn test_escape_copy_text_plain() {
        assert_eq!(escape_copy_text("hello world"), "hello world");
    }

    #[test]
    fn test_escape_copy_text_tab() {
        assert_eq!(escape_copy_text("a\tb"), "a\\tb");
    }

    #[test]
    fn test_escape_copy_text_newline() {
        assert_eq!(escape_copy_text("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn test_escape_copy_text_carriage_return() {
        assert_eq!(escape_copy_text("a\rb"), "a\\rb");
    }

    #[test]
    fn test_escape_copy_text_backslash() {
        assert_eq!(escape_copy_text("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_escape_copy_text_mixed() {
        assert_eq!(
            escape_copy_text("line1\nline2\ttab\\slash"),
            "line1\\nline2\\ttab\\\\slash"
        );
    }

    // -- copy_line tests --

    #[test]
    fn test_copy_line_all_values() {
        let line = copy_line(&[Some("1001"), Some("Test Title"), Some("US")]);
        assert_eq!(line, "1001\tTest Title\tUS\n");
    }

    #[test]
    fn test_copy_line_with_nulls() {
        let line = copy_line(&[Some("1001"), None, Some("US")]);
        assert_eq!(line, "1001\t\\N\tUS\n");
    }

    #[test]
    fn test_copy_line_empty_string_becomes_null() {
        let line = copy_line(&[Some("1001"), Some(""), Some("US")]);
        assert_eq!(line, "1001\t\\N\tUS\n");
    }

    #[test]
    fn test_copy_line_with_special_chars() {
        let line = copy_line(&[Some("1"), Some("Title with\ttab"), Some("Note\nline2")]);
        assert_eq!(line, "1\tTitle with\\ttab\tNote\\nline2\n");
    }

    // -- artwork tests --

    #[test]
    fn test_artwork_primary_preferred() {
        let images = vec![
            ReleaseImage {
                image_type: "secondary".to_string(),
                width: 300,
                height: 300,
                uri: "https://img.discogs.com/secondary.jpg".to_string(),
            },
            ReleaseImage {
                image_type: "primary".to_string(),
                width: 600,
                height: 600,
                uri: "https://img.discogs.com/primary.jpg".to_string(),
            },
        ];
        assert_eq!(
            pick_artwork_url(&images),
            Some("https://img.discogs.com/primary.jpg")
        );
    }

    #[test]
    fn test_artwork_fallback_to_first() {
        let images = vec![
            ReleaseImage {
                image_type: "secondary".to_string(),
                width: 300,
                height: 300,
                uri: "https://img.discogs.com/secondary.jpg".to_string(),
            },
            ReleaseImage {
                image_type: "secondary".to_string(),
                width: 600,
                height: 600,
                uri: "https://img.discogs.com/another.jpg".to_string(),
            },
        ];
        assert_eq!(
            pick_artwork_url(&images),
            Some("https://img.discogs.com/secondary.jpg")
        );
    }

    #[test]
    fn test_artwork_no_images() {
        let images: Vec<ReleaseImage> = vec![];
        assert_eq!(pick_artwork_url(&images), None);
    }

    // -- dedup tests --

    #[test]
    fn test_dedup_release_artist() {
        let mut dedup = ArtistDedup::new();
        assert!(dedup.insert(1001, "Autechre"));
        assert!(dedup.insert(1001, "Boards of Canada"));
        // Duplicate
        assert!(!dedup.insert(1001, "Autechre"));
        // Same artist, different release
        assert!(dedup.insert(1002, "Autechre"));
    }

    #[test]
    fn test_dedup_track_artist() {
        let mut dedup = TrackArtistDedup::new();
        assert!(dedup.insert(1001, 1, "Autechre"));
        assert!(dedup.insert(1001, 2, "Autechre"));
        // Duplicate
        assert!(!dedup.insert(1001, 1, "Autechre"));
        // Same release+track, different artist
        assert!(dedup.insert(1001, 1, "Boards of Canada"));
    }

    #[test]
    fn test_dedup_label() {
        let mut dedup = LabelDedup::new();
        assert!(dedup.insert(1001, "Warp"));
        assert!(dedup.insert(1001, "4AD"));
        // Duplicate
        assert!(!dedup.insert(1001, "Warp"));
        // Same label, different release
        assert!(dedup.insert(1002, "Warp"));
    }

    // -- PgOutput integration tests (require TEST_DATABASE_URL) --

    /// Helper to get a test DB connection, or skip the test if unavailable.
    fn test_db_url() -> Option<String> {
        std::env::var("TEST_DATABASE_URL").ok()
    }

    /// Set up a clean schema for testing. Drops and recreates all tables.
    fn set_up_test_schema(client: &mut postgres::Client) {
        // Drop tables in reverse FK order
        client
            .batch_execute(
                "DROP TABLE IF EXISTS cache_metadata CASCADE;
                 DROP TABLE IF EXISTS release_track_count CASCADE;
                 DROP TABLE IF EXISTS release_track_artist CASCADE;
                 DROP TABLE IF EXISTS release_track CASCADE;
                 DROP TABLE IF EXISTS release_label CASCADE;
                 DROP TABLE IF EXISTS release_artist CASCADE;
                 DROP TABLE IF EXISTS release CASCADE;",
            )
            .unwrap();

        // Create tables matching discogs-cache schema
        client
            .batch_execute(
                "CREATE TABLE release (
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
                    label_name text NOT NULL
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
                CREATE TABLE cache_metadata (
                    release_id integer PRIMARY KEY REFERENCES release(id) ON DELETE CASCADE,
                    cached_at timestamptz NOT NULL DEFAULT now(),
                    source text NOT NULL,
                    last_validated timestamptz
                );",
            )
            .unwrap();
    }

    fn sample_release() -> crate::model::Release {
        use crate::model::*;
        Release {
            id: 1001,
            status: "Accepted".to_string(),
            title: "Confield".to_string(),
            country: "UK".to_string(),
            released: "2001-04-30".to_string(),
            notes: "".to_string(),
            data_quality: "Correct".to_string(),
            master_id: Some(500),
            formats: vec![Format {
                name: "CD".to_string(),
                qty: 1,
            }],
            artists: vec![ReleaseArtist {
                artist_id: 1,
                name: "Autechre".to_string(),
                anv: "".to_string(),
                join_field: "".to_string(),
                position: 1,
            }],
            extra_artists: vec![],
            labels: vec![ReleaseLabel {
                name: "Warp".to_string(),
                catno: "WARPCD77".to_string(),
            }],
            tracks: vec![
                ReleaseTrack {
                    position: "1".to_string(),
                    title: "VI Scose Poise".to_string(),
                    duration: "7:36".to_string(),
                    artists: vec![],
                    extra_artists: vec![],
                },
                ReleaseTrack {
                    position: "2".to_string(),
                    title: "Cfern".to_string(),
                    duration: "5:16".to_string(),
                    artists: vec![],
                    extra_artists: vec![],
                },
            ],
            images: vec![ReleaseImage {
                image_type: "primary".to_string(),
                width: 600,
                height: 600,
                uri: "https://img.discogs.com/confield.jpg".to_string(),
            }],
        }
    }

    #[test]
    fn test_pg_output_single_release() {
        let db_url = match test_db_url() {
            Some(url) => url,
            None => return,
        };

        let mut setup_client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
        set_up_test_schema(&mut setup_client);
        drop(setup_client);

        let mut output = PgOutput::new(&db_url, 10000).unwrap();
        output.write_release(&sample_release()).unwrap();
        output.finish().unwrap();

        let mut client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();

        // Verify release
        let row = client
            .query_one(
                "SELECT id, title, release_year, country, master_id FROM release",
                &[],
            )
            .unwrap();
        assert_eq!(row.get::<_, i32>(0), 1001);
        assert_eq!(row.get::<_, &str>(1), "Confield");
        assert_eq!(row.get::<_, Option<i16>>(2), Some(2001));
        assert_eq!(row.get::<_, Option<&str>>(3), Some("UK"));
        assert_eq!(row.get::<_, Option<i32>>(4), Some(500));

        // Verify release_artist
        let rows = client
            .query(
                "SELECT release_id, artist_id, artist_name, extra FROM release_artist",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, &str>(2), "Autechre");
        assert_eq!(rows[0].get::<_, Option<i32>>(3), Some(0));

        // Verify release_label
        let rows = client
            .query("SELECT release_id, label_name FROM release_label", &[])
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, &str>(1), "Warp");

        // Verify release_track
        let rows = client
            .query(
                "SELECT release_id, sequence, title FROM release_track ORDER BY sequence",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, &str>(2), "VI Scose Poise");
        assert_eq!(rows[1].get::<_, &str>(2), "Cfern");

        // Verify artwork_url was set
        let row = client
            .query_one("SELECT artwork_url FROM release WHERE id = 1001", &[])
            .unwrap();
        assert_eq!(
            row.get::<_, Option<&str>>(0),
            Some("https://img.discogs.com/confield.jpg")
        );

        // Verify cache_metadata
        let row = client
            .query_one(
                "SELECT source FROM cache_metadata WHERE release_id = 1001",
                &[],
            )
            .unwrap();
        assert_eq!(row.get::<_, &str>(0), "bulk_import");

        // Verify release_track_count
        let row = client
            .query_one(
                "SELECT track_count FROM release_track_count WHERE release_id = 1001",
                &[],
            )
            .unwrap();
        assert_eq!(row.get::<_, i32>(0), 2);
    }

    #[test]
    fn test_pg_output_batch_boundary() {
        let db_url = match test_db_url() {
            Some(url) => url,
            None => return,
        };

        let mut setup_client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
        set_up_test_schema(&mut setup_client);
        drop(setup_client);

        // Use batch_size=2 to trigger a flush mid-import
        let mut output = PgOutput::new(&db_url, 2).unwrap();

        for i in 1..=5 {
            let release = crate::model::Release {
                id: i,
                title: format!("Release {}", i),
                artists: vec![crate::model::ReleaseArtist {
                    artist_id: 1,
                    name: "Artist".to_string(),
                    position: 1,
                    ..Default::default()
                }],
                ..Default::default()
            };
            output.write_release(&release).unwrap();
        }
        // batch_size=2: flushes at 2, 4; finish() flushes remaining 1
        assert_eq!(output.total_written(), 4);
        output.finish().unwrap();
        assert_eq!(output.total_written(), 5);

        let mut client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
        let row = client
            .query_one("SELECT count(*) FROM release", &[])
            .unwrap();
        assert_eq!(row.get::<_, i64>(0), 5);
    }

    #[test]
    fn test_pg_output_dedup() {
        let db_url = match test_db_url() {
            Some(url) => url,
            None => return,
        };

        let mut setup_client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
        set_up_test_schema(&mut setup_client);
        drop(setup_client);

        let mut output = PgOutput::new(&db_url, 10000).unwrap();

        // Release with duplicate artist and duplicate label
        let release = crate::model::Release {
            id: 1,
            title: "Test".to_string(),
            artists: vec![
                crate::model::ReleaseArtist {
                    artist_id: 1,
                    name: "Autechre".to_string(),
                    position: 1,
                    ..Default::default()
                },
                crate::model::ReleaseArtist {
                    artist_id: 1,
                    name: "Autechre".to_string(),
                    position: 2,
                    ..Default::default()
                },
            ],
            labels: vec![
                crate::model::ReleaseLabel {
                    name: "Warp".to_string(),
                    catno: "A".to_string(),
                },
                crate::model::ReleaseLabel {
                    name: "Warp".to_string(),
                    catno: "B".to_string(),
                },
            ],
            ..Default::default()
        };
        output.write_release(&release).unwrap();
        output.finish().unwrap();

        let mut client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();

        // Only 1 artist row despite 2 in input (dedup by release_id + artist_name)
        let row = client
            .query_one("SELECT count(*) FROM release_artist", &[])
            .unwrap();
        assert_eq!(row.get::<_, i64>(0), 1);

        // Only 1 label row despite 2 in input (dedup by release_id + label_name)
        let row = client
            .query_one("SELECT count(*) FROM release_label", &[])
            .unwrap();
        assert_eq!(row.get::<_, i64>(0), 1);
    }

    #[test]
    fn test_pg_output_required_skipped() {
        let db_url = match test_db_url() {
            Some(url) => url,
            None => return,
        };

        let mut setup_client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
        set_up_test_schema(&mut setup_client);
        drop(setup_client);

        let mut output = PgOutput::new(&db_url, 10000).unwrap();

        // Release with empty title should be skipped
        let release = crate::model::Release {
            id: 1,
            title: "".to_string(),
            artists: vec![crate::model::ReleaseArtist {
                artist_id: 1,
                name: "Artist".to_string(),
                position: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        output.write_release(&release).unwrap();
        output.finish().unwrap();

        let mut client = postgres::Client::connect(&db_url, postgres::NoTls).unwrap();
        let row = client
            .query_one("SELECT count(*) FROM release", &[])
            .unwrap();
        assert_eq!(row.get::<_, i64>(0), 0);
    }
}
