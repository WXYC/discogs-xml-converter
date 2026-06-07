//! CSV output writer for Discogs artist data.
//!
//! Writes 5 CSV files:
//! - artist.csv (artist_id, artist_name, profile)
//! - artist_alias.csv (artist_id, artist_name, alias_name) — Discogs `<aliases>` (alter egos)
//! - artist_name_variation.csv (artist_id, name) — Discogs `<namevariations>` (spelling variants)
//! - artist_member.csv (group_artist_id, group_name, member_artist_id, member_name)
//! - artist_url.csv (artist_id, url) — Discogs `<urls>` (external links: Wikipedia, official sites, social)
//!
//! Aliases and name variations are distinct in Discogs and are stored in
//! separate tables on the consumer side (`artist_alias` and
//! `artist_name_variation`). Folding them into one CSV used to leave
//! `artist_name_variation` empty after a rebuild — see WXYC/discogs-etl#215.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use csv::Writer;

use crate::artist_model::Artist;

/// Manages CSV writers for artist, alias, name-variation, member, and url output.
pub struct ArtistCsvOutput {
    artist: Writer<fs::File>,
    alias: Writer<fs::File>,
    name_variation: Writer<fs::File>,
    member: Writer<fs::File>,
    url: Writer<fs::File>,
}

impl ArtistCsvOutput {
    /// Create a new ArtistCsvOutput, writing headers to all files.
    pub fn new(output_dir: &Path) -> Result<Self> {
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        let mut artist = Self::create_writer(output_dir, "artist.csv")?;
        artist.write_record(["artist_id", "artist_name", "profile"])?;

        let mut alias = Self::create_writer(output_dir, "artist_alias.csv")?;
        alias.write_record(["artist_id", "artist_name", "alias_name"])?;

        let mut name_variation = Self::create_writer(output_dir, "artist_name_variation.csv")?;
        name_variation.write_record(["artist_id", "name"])?;

        let mut member = Self::create_writer(output_dir, "artist_member.csv")?;
        member.write_record([
            "group_artist_id",
            "group_name",
            "member_artist_id",
            "member_name",
        ])?;

        let mut url = Self::create_writer(output_dir, "artist_url.csv")?;
        url.write_record(["artist_id", "url"])?;

        Ok(ArtistCsvOutput {
            artist,
            alias,
            name_variation,
            member,
            url,
        })
    }

    fn create_writer(dir: &Path, filename: &str) -> Result<Writer<fs::File>> {
        let path = dir.join(filename);
        let file = fs::File::create(&path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Writer::from_writer(file))
    }

    /// Write an artist's aliases, name variations, and members to CSV files.
    pub fn write_artist(&mut self, artist: &Artist) -> Result<()> {
        let id_str = artist.id.to_string();

        if !artist.profile.is_empty() {
            self.artist
                .write_record([&id_str, &artist.name, &artist.profile])?;
        }

        for nv in &artist.name_variations {
            self.name_variation.write_record([&id_str, nv])?;
        }

        for alias in &artist.aliases {
            self.alias.write_record([&id_str, &artist.name, alias])?;
        }

        for member in &artist.members {
            self.member.write_record([
                &id_str,
                &artist.name,
                &member.id.to_string(),
                &member.name,
            ])?;
        }

        for url in &artist.urls {
            self.url.write_record([&id_str, url])?;
        }

        Ok(())
    }

    /// Flush all writers.
    pub fn flush(&mut self) -> Result<()> {
        self.artist.flush()?;
        self.alias.flush()?;
        self.name_variation.flush()?;
        self.member.flush()?;
        self.url.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artist_model::*;

    fn sample_artist() -> Artist {
        Artist {
            id: 123,
            name: "P. Diddy".to_string(),
            profile: "American rapper, singer, record producer, and entrepreneur.".to_string(),
            name_variations: vec!["P Diddy".to_string(), "Puff Daddy".to_string()],
            aliases: vec!["Sean Combs".to_string(), "Diddy".to_string()],
            members: vec![Member {
                id: 1001,
                name: "Member One".to_string(),
            }],
            urls: vec![
                "https://en.wikipedia.org/wiki/Sean_Combs".to_string(),
                "https://www.diddy.com/".to_string(),
            ],
        }
    }

    #[test]
    fn test_write_artist_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        output.write_artist(&sample_artist()).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("artist.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "123");
        assert_eq!(&records[0][1], "P. Diddy");
        assert_eq!(
            &records[0][2],
            "American rapper, singer, record producer, and entrepreneur."
        );
    }

    #[test]
    fn test_skip_empty_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        let artist = Artist {
            id: 999,
            name: "No Profile".to_string(),
            profile: String::new(),
            ..Default::default()
        };
        output.write_artist(&artist).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("artist.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn test_csv_headers() {
        let dir = tempfile::tempdir().unwrap();
        let output = ArtistCsvOutput::new(dir.path()).unwrap();
        drop(output);

        let mut rdr = csv::Reader::from_path(dir.path().join("artist_alias.csv")).unwrap();
        let headers: Vec<String> = rdr
            .headers()
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(headers, vec!["artist_id", "artist_name", "alias_name"]);

        let mut rdr = csv::Reader::from_path(dir.path().join("artist_member.csv")).unwrap();
        let headers: Vec<String> = rdr
            .headers()
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            headers,
            vec![
                "group_artist_id",
                "group_name",
                "member_artist_id",
                "member_name"
            ]
        );
    }

    #[test]
    fn test_write_artist_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        output.write_artist(&sample_artist()).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("artist_alias.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        // 2 aliases ONLY; name variations now land in artist_name_variation.csv.
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "123");
        assert_eq!(&records[0][1], "P. Diddy");
        assert_eq!(&records[0][2], "Sean Combs");
        assert_eq!(&records[1][2], "Diddy");
    }

    #[test]
    fn test_write_artist_name_variations() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        output.write_artist(&sample_artist()).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("artist_name_variation.csv")).unwrap();
        let headers: Vec<String> = rdr
            .headers()
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(headers, vec!["artist_id", "name"]);

        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "123");
        assert_eq!(&records[0][1], "P Diddy");
        assert_eq!(&records[1][0], "123");
        assert_eq!(&records[1][1], "Puff Daddy");
    }

    #[test]
    fn test_name_variations_isolated_from_aliases() {
        // Pin: name_variations and aliases must not cross-pollinate. If the
        // converter ever folds them back together, the artist_name_variation
        // table will silently stay empty on the consumer (WXYC/discogs-etl#215).
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        let artist = Artist {
            id: 7,
            name: "Solo".to_string(),
            profile: String::new(),
            name_variations: vec!["Solo (var)".to_string()],
            aliases: vec![],
            members: vec![],
            urls: vec![],
        };
        output.write_artist(&artist).unwrap();
        output.flush().unwrap();

        let aliases: Vec<csv::StringRecord> =
            csv::Reader::from_path(dir.path().join("artist_alias.csv"))
                .unwrap()
                .records()
                .map(|r| r.unwrap())
                .collect();
        assert!(
            aliases.is_empty(),
            "name_variation must not leak into artist_alias"
        );

        let nvs: Vec<csv::StringRecord> =
            csv::Reader::from_path(dir.path().join("artist_name_variation.csv"))
                .unwrap()
                .records()
                .map(|r| r.unwrap())
                .collect();
        assert_eq!(nvs.len(), 1);
        assert_eq!(&nvs[0][1], "Solo (var)");
    }

    #[test]
    fn test_write_artist_urls() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        output.write_artist(&sample_artist()).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("artist_url.csv")).unwrap();
        let headers: Vec<String> = rdr
            .headers()
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(headers, vec!["artist_id", "url"]);

        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "123");
        assert_eq!(&records[0][1], "https://en.wikipedia.org/wiki/Sean_Combs");
        assert_eq!(&records[1][0], "123");
        assert_eq!(&records[1][1], "https://www.diddy.com/");
    }

    #[test]
    fn test_skip_empty_urls() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        let artist = Artist {
            id: 42,
            name: "No URLs".to_string(),
            urls: vec![],
            ..Default::default()
        };
        output.write_artist(&artist).unwrap();
        output.flush().unwrap();

        let records: Vec<csv::StringRecord> =
            csv::Reader::from_path(dir.path().join("artist_url.csv"))
                .unwrap()
                .records()
                .map(|r| r.unwrap())
                .collect();
        assert!(records.is_empty());
    }

    #[test]
    fn test_write_artist_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = ArtistCsvOutput::new(dir.path()).unwrap();
        output.write_artist(&sample_artist()).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("artist_member.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "123");
        assert_eq!(&records[0][1], "P. Diddy");
        assert_eq!(&records[0][2], "1001");
        assert_eq!(&records[0][3], "Member One");
    }
}
