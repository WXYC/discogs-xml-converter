//! PostgreSQL direct output for release data.
//!
//! Provides `PgOutput`, which implements `ReleaseOutput` to stream releases
//! directly into PostgreSQL via COPY, eliminating the CSV round-trip.
//!
//! COPY TEXT escaping, batch buffering, deduplication, and helper functions
//! are provided by the `wxyc-etl` crate. Domain-specific transforms (artwork
//! URL population, track count table, cache_metadata insertion) remain local.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Context, Result};
use log::info;
use wxyc_etl::pg::{
    escape_copy_text_into, extract_year, pick_artwork_url, write_copy_int, ArtistDedup,
    BatchCopier, LabelDedup, TrackArtistDedup,
};

use crate::model::Release;
use crate::output::ReleaseOutput;

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
    copier: BatchCopier,
    artist_dedup: ArtistDedup,
    label_dedup: LabelDedup,
    track_artist_dedup: TrackArtistDedup,
    artwork: ArtworkMap,
    track_counts: TrackCountMap,
}

impl PgOutput {
    /// Connect to PostgreSQL and prepare for streaming release data.
    pub fn new(database_url: &str, batch_size: usize) -> Result<Self> {
        let client = postgres::Client::connect(database_url, postgres::NoTls)
            .with_context(|| format!("Failed to connect to PostgreSQL at {}", database_url))?;

        let copier = BatchCopier::new(
            &[
                (
                    "release",
                    "COPY release (id, title, release_year, country, master_id) FROM STDIN",
                ),
                (
                    "release_artist",
                    "COPY release_artist (release_id, artist_id, artist_name, extra) FROM STDIN",
                ),
                (
                    "release_label",
                    "COPY release_label (release_id, label_name) FROM STDIN",
                ),
                (
                    "release_track",
                    "COPY release_track (release_id, sequence, position, title, duration) FROM STDIN",
                ),
                (
                    "release_track_artist",
                    "COPY release_track_artist (release_id, track_sequence, artist_name) FROM STDIN",
                ),
                (
                    "release_genre",
                    "COPY release_genre (release_id, genre) FROM STDIN",
                ),
                (
                    "release_style",
                    "COPY release_style (release_id, style) FROM STDIN",
                ),
                (
                    "release_company",
                    "COPY release_company (release_id, company_id, company_name, entity_type, entity_type_name) FROM STDIN",
                ),
            ],
            batch_size,
        );

        Ok(Self {
            client,
            copier,
            artist_dedup: ArtistDedup::new(),
            label_dedup: LabelDedup::new(),
            track_artist_dedup: TrackArtistDedup::new(),
            artwork: HashMap::new(),
            track_counts: HashMap::new(),
        })
    }

    /// Number of releases flushed to PostgreSQL so far.
    pub fn total_written(&self) -> usize {
        self.copier.total_written()
    }
}

impl ReleaseOutput for PgOutput {
    fn write_release(&mut self, release: &Release) -> Result<()> {
        // Skip releases with empty title (required field)
        if release.title.is_empty() {
            return Ok(());
        }

        // release row: id, title, release_year, country, master_id
        {
            let buf = self.copier.buffer("release");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            escape_copy_text_into(buf, &release.title);
            buf.push(b'\t');
            match extract_year(&release.released) {
                Some(y) => write_copy_int(buf, y),
                None => buf.extend_from_slice(b"\\N"),
            }
            buf.push(b'\t');
            if release.country.is_empty() {
                buf.extend_from_slice(b"\\N");
            } else {
                escape_copy_text_into(buf, &release.country);
            }
            buf.push(b'\t');
            match release.master_id {
                Some(id) => write_copy_int(buf, id),
                None => buf.extend_from_slice(b"\\N"),
            }
            buf.push(b'\n');
        }

        // release_artist rows (main artists, extra=0)
        for artist in &release.artists {
            if artist.name.is_empty() {
                continue;
            }
            if !self
                .artist_dedup
                .insert((release.id, artist.name.to_string()))
            {
                continue;
            }
            let buf = self.copier.buffer("release_artist");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            write_copy_int(buf, artist.artist_id);
            buf.push(b'\t');
            escape_copy_text_into(buf, &artist.name);
            buf.extend_from_slice(b"\t0\n");
        }

        // release_artist rows (extra artists, extra=1)
        for artist in &release.extra_artists {
            if artist.name.is_empty() {
                continue;
            }
            if !self
                .artist_dedup
                .insert((release.id, artist.name.to_string()))
            {
                continue;
            }
            let buf = self.copier.buffer("release_artist");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            write_copy_int(buf, artist.artist_id);
            buf.push(b'\t');
            escape_copy_text_into(buf, &artist.name);
            buf.extend_from_slice(b"\t1\n");
        }

        // release_label rows (release_id, label_name only -- catno is not in the DB)
        for label in &release.labels {
            if label.name.is_empty() {
                continue;
            }
            if !self
                .label_dedup
                .insert((release.id, label.name.to_string()))
            {
                continue;
            }
            let buf = self.copier.buffer("release_label");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            escape_copy_text_into(buf, &label.name);
            buf.push(b'\n');
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

            // Write track row directly
            {
                let buf = self.copier.buffer("release_track");
                write_copy_int(buf, release.id);
                buf.push(b'\t');
                write_copy_int(buf, seq);
                buf.push(b'\t');
                if track.position.is_empty() {
                    buf.extend_from_slice(b"\\N");
                } else {
                    escape_copy_text_into(buf, &track.position);
                }
                buf.push(b'\t');
                escape_copy_text_into(buf, &track.title);
                buf.push(b'\t');
                if track.duration.is_empty() {
                    buf.extend_from_slice(b"\\N");
                } else {
                    escape_copy_text_into(buf, &track.duration);
                }
                buf.push(b'\n');
            }

            // Track artists (both main and extra)
            for artist in track.artists.iter().chain(track.extra_artists.iter()) {
                if !self
                    .track_artist_dedup
                    .insert((release.id, seq, artist.name.to_string()))
                {
                    continue;
                }
                let buf = self.copier.buffer("release_track_artist");
                write_copy_int(buf, release.id);
                buf.push(b'\t');
                write_copy_int(buf, seq);
                buf.push(b'\t');
                escape_copy_text_into(buf, &artist.name);
                buf.push(b'\n');
            }
        }

        // release_genre rows
        for genre in &release.genres {
            if genre.is_empty() {
                continue;
            }
            let buf = self.copier.buffer("release_genre");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            escape_copy_text_into(buf, genre);
            buf.push(b'\n');
        }

        // release_style rows
        for style in &release.styles {
            if style.is_empty() {
                continue;
            }
            let buf = self.copier.buffer("release_style");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            escape_copy_text_into(buf, style);
            buf.push(b'\n');
        }

        // release_company rows
        for company in &release.companies {
            if company.name.is_empty() {
                continue;
            }
            let buf = self.copier.buffer("release_company");
            write_copy_int(buf, release.id);
            buf.push(b'\t');
            write_copy_int(buf, company.company_id);
            buf.push(b'\t');
            escape_copy_text_into(buf, &company.name);
            buf.push(b'\t');
            write_copy_int(buf, company.entity_type);
            buf.push(b'\t');
            escape_copy_text_into(buf, &company.entity_type_name);
            buf.push(b'\n');
        }

        // Artwork (accumulated for batch UPDATE in finish())
        if let Some(url) = pick_artwork_url(&release.images) {
            self.artwork.insert(release.id, url.to_string());
        }

        // Flush if batch is full
        self.copier.count_and_maybe_flush(&mut self.client)?;

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.copier.flush(&mut self.client)
    }

    fn finish(&mut self) -> Result<()> {
        // 1. Flush remaining buffered rows
        self.copier.flush(&mut self.client)?;

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
                let mut buf = Vec::new();
                for (release_id, url) in &self.artwork {
                    buf.clear();
                    write_copy_int(&mut buf, *release_id);
                    buf.push(b'\t');
                    escape_copy_text_into(&mut buf, url);
                    buf.push(b'\n');
                    writer.write_all(&buf)?;
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
            let mut buf = Vec::new();
            for (release_id, count) in &self.track_counts {
                buf.clear();
                write_copy_int(&mut buf, *release_id);
                buf.push(b'\t');
                write_copy_int(&mut buf, *count);
                buf.push(b'\n');
                writer.write_all(&buf)?;
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
    use wxyc_etl::pg::{copy_line, empty_to_none, escape_copy_text, write_copy_row};

    // -- extract_year tests (now delegating to wxyc_etl) --

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

    // -- escape_copy_text_into tests --

    #[test]
    fn test_escape_copy_text_into_plain() {
        let mut buf = Vec::new();
        wxyc_etl::pg::escape_copy_text_into(&mut buf, "hello world");
        assert_eq!(buf, b"hello world");
    }

    #[test]
    fn test_escape_copy_text_into_special_chars() {
        let mut buf = Vec::new();
        wxyc_etl::pg::escape_copy_text_into(&mut buf, "line1\nline2\ttab\\slash\rret");
        assert_eq!(buf, b"line1\\nline2\\ttab\\\\slash\\rret");
    }

    #[test]
    fn test_escape_copy_text_into_matches_escape_copy_text() {
        let cases = ["hello", "a\tb", "a\nb", "a\\b", "a\rb", "mix\t\n\\end"];
        for s in &cases {
            let mut buf = Vec::new();
            wxyc_etl::pg::escape_copy_text_into(&mut buf, s);
            assert_eq!(
                String::from_utf8(buf).unwrap(),
                escape_copy_text(s),
                "Mismatch for input: {:?}",
                s,
            );
        }
    }

    // -- write_copy_row tests --

    #[test]
    fn test_write_copy_row_all_values() {
        let mut buf = Vec::new();
        write_copy_row(&mut buf, &[Some("1001"), Some("Test Title"), Some("US")]);
        assert_eq!(buf, b"1001\tTest Title\tUS\n");
    }

    #[test]
    fn test_write_copy_row_with_nulls() {
        let mut buf = Vec::new();
        write_copy_row(&mut buf, &[Some("1001"), None, Some("US")]);
        assert_eq!(buf, b"1001\t\\N\tUS\n");
    }

    #[test]
    fn test_write_copy_row_empty_string_becomes_null() {
        let mut buf = Vec::new();
        write_copy_row(&mut buf, &[Some("1001"), Some(""), Some("US")]);
        assert_eq!(buf, b"1001\t\\N\tUS\n");
    }

    #[test]
    fn test_write_copy_row_with_special_chars() {
        let mut buf = Vec::new();
        write_copy_row(
            &mut buf,
            &[Some("1"), Some("Title with\ttab"), Some("Note\nline2")],
        );
        assert_eq!(buf, b"1\tTitle with\\ttab\tNote\\nline2\n");
    }

    #[test]
    fn test_write_copy_row_matches_copy_line() {
        let test_cases: Vec<Vec<Option<&str>>> = vec![
            vec![Some("1001"), Some("Test"), Some("US")],
            vec![Some("1"), None, Some("value")],
            vec![Some("42"), Some(""), Some("end")],
            vec![Some("1"), Some("tab\there"), Some("nl\nhere")],
        ];
        for values in &test_cases {
            let mut buf = Vec::new();
            write_copy_row(&mut buf, values);
            let expected = copy_line(values);
            assert_eq!(
                String::from_utf8(buf).unwrap(),
                expected,
                "Mismatch for values: {:?}",
                values,
            );
        }
    }

    // -- write_copy_int tests --

    #[test]
    fn test_write_copy_int_u64() {
        let mut buf = Vec::new();
        wxyc_etl::pg::write_copy_int(&mut buf, 12345u64);
        assert_eq!(buf, b"12345");
    }

    #[test]
    fn test_write_copy_int_i16() {
        let mut buf = Vec::new();
        wxyc_etl::pg::write_copy_int(&mut buf, 2001i16);
        assert_eq!(buf, b"2001");
    }

    #[test]
    fn test_write_copy_int_u32() {
        let mut buf = Vec::new();
        wxyc_etl::pg::write_copy_int(&mut buf, 0u32);
        assert_eq!(buf, b"0");
    }

    // -- artwork tests --

    use super::*;
    use crate::model::ReleaseImage;

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

    // -- dedup tests (now using wxyc_etl types) --

    #[test]
    fn test_dedup_release_artist() {
        let mut dedup = ArtistDedup::new();
        assert!(dedup.insert((1001, "Autechre".to_string())));
        assert!(dedup.insert((1001, "Boards of Canada".to_string())));
        // Duplicate
        assert!(!dedup.insert((1001, "Autechre".to_string())));
        // Same artist, different release
        assert!(dedup.insert((1002, "Autechre".to_string())));
    }

    #[test]
    fn test_dedup_track_artist() {
        let mut dedup = TrackArtistDedup::new();
        assert!(dedup.insert((1001, 1, "Autechre".to_string())));
        assert!(dedup.insert((1001, 2, "Autechre".to_string())));
        // Duplicate
        assert!(!dedup.insert((1001, 1, "Autechre".to_string())));
        // Same release+track, different artist
        assert!(dedup.insert((1001, 1, "Boards of Canada".to_string())));
    }

    #[test]
    fn test_dedup_label() {
        let mut dedup = LabelDedup::new();
        assert!(dedup.insert((1001, "Warp".to_string())));
        assert!(dedup.insert((1001, "4AD".to_string())));
        // Duplicate
        assert!(!dedup.insert((1001, "Warp".to_string())));
        // Same label, different release
        assert!(dedup.insert((1002, "Warp".to_string())));
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
                 DROP TABLE IF EXISTS release_genre CASCADE;
                 DROP TABLE IF EXISTS release_style CASCADE;
                 DROP TABLE IF EXISTS release_company CASCADE;
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
            genres: vec!["Electronic".to_string()],
            styles: vec!["IDM".to_string(), "Abstract".to_string()],
            companies: vec![crate::model::ReleaseCompany {
                company_id: 271046,
                name: "The Globe Studios".to_string(),
                entity_type: 23,
                entity_type_name: "Recorded At".to_string(),
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

        // Verify release_genre
        let rows = client
            .query(
                "SELECT genre FROM release_genre WHERE release_id = 1001 ORDER BY genre",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, &str>(0), "Electronic");

        // Verify release_style
        let rows = client
            .query(
                "SELECT style FROM release_style WHERE release_id = 1001 ORDER BY style",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, &str>(0), "Abstract");
        assert_eq!(rows[1].get::<_, &str>(0), "IDM");

        // Verify release_company
        let rows = client
            .query(
                "SELECT company_id, company_name, entity_type, entity_type_name FROM release_company WHERE release_id = 1001",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, Option<i32>>(0), Some(271046));
        assert_eq!(rows[0].get::<_, &str>(1), "The Globe Studios");
        assert_eq!(rows[0].get::<_, Option<i32>>(2), Some(23));
        assert_eq!(rows[0].get::<_, &str>(3), "Recorded At");
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
