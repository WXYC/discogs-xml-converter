//! CSV output writer for Discogs artist data.
//!
//! Writes 2 CSV files:
//! - artist_alias.csv (artist_id, artist_name, alias_name)
//! - artist_member.csv (group_artist_id, group_name, member_artist_id, member_name)

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use csv::Writer;

use crate::artist_model::Artist;

/// Manages CSV writers for artist alias and member output.
pub struct ArtistCsvOutput {
    alias: Writer<fs::File>,
    member: Writer<fs::File>,
}

impl ArtistCsvOutput {
    /// Create a new ArtistCsvOutput, writing headers to both files.
    pub fn new(output_dir: &Path) -> Result<Self> {
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        let mut alias = Self::create_writer(output_dir, "artist_alias.csv")?;
        alias.write_record(["artist_id", "artist_name", "alias_name"])?;

        let mut member = Self::create_writer(output_dir, "artist_member.csv")?;
        member.write_record([
            "group_artist_id",
            "group_name",
            "member_artist_id",
            "member_name",
        ])?;

        Ok(ArtistCsvOutput { alias, member })
    }

    fn create_writer(dir: &Path, filename: &str) -> Result<Writer<fs::File>> {
        let path = dir.join(filename);
        let file = fs::File::create(&path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Writer::from_writer(file))
    }

    /// Write an artist's aliases, name variations, and members to CSV files.
    ///
    /// Both aliases and name variations are written to artist_alias.csv since
    /// they serve the same purpose: alternate names an artist might be credited under.
    pub fn write_artist(&mut self, artist: &Artist) -> Result<()> {
        let id_str = artist.id.to_string();

        // Write name variations as aliases
        for nv in &artist.name_variations {
            self.alias.write_record([&id_str, &artist.name, nv])?;
        }

        // Write aliases
        for alias in &artist.aliases {
            self.alias.write_record([&id_str, &artist.name, alias])?;
        }

        // Write members
        for member in &artist.members {
            self.member.write_record([
                &id_str,
                &artist.name,
                &member.id.to_string(),
                &member.name,
            ])?;
        }

        Ok(())
    }

    /// Flush all writers.
    pub fn flush(&mut self) -> Result<()> {
        self.alias.flush()?;
        self.member.flush()?;
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
            name_variations: vec!["P Diddy".to_string(), "Puff Daddy".to_string()],
            aliases: vec!["Sean Combs".to_string(), "Diddy".to_string()],
            members: vec![Member {
                id: 1001,
                name: "Member One".to_string(),
            }],
        }
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
        // 2 name variations + 2 aliases = 4
        assert_eq!(records.len(), 4);
        assert_eq!(&records[0][0], "123");
        assert_eq!(&records[0][1], "P. Diddy");
        assert_eq!(&records[0][2], "P Diddy");
        assert_eq!(&records[1][2], "Puff Daddy");
        assert_eq!(&records[2][2], "Sean Combs");
        assert_eq!(&records[3][2], "Diddy");
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
