//! CSV output writer for Discogs master release data.
//!
//! Writes master.csv and master_artist.csv from parsed master data.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use csv::Writer;

use crate::master_model::Master;

/// Manages CSV writers for master and master_artist output.
pub struct MasterCsvOutput {
    master: Writer<fs::File>,
    master_artist: Writer<fs::File>,
}

impl MasterCsvOutput {
    /// Create a new MasterCsvOutput, writing headers.
    pub fn new(output_dir: &Path) -> Result<Self> {
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        let mut master = Self::create_writer(output_dir, "master.csv")?;
        master.write_record(["id", "title", "main_release_id", "year"])?;

        let mut master_artist = Self::create_writer(output_dir, "master_artist.csv")?;
        master_artist.write_record(["master_id", "artist_id", "artist_name"])?;

        Ok(MasterCsvOutput {
            master,
            master_artist,
        })
    }

    fn create_writer(dir: &Path, filename: &str) -> Result<Writer<fs::File>> {
        let path = dir.join(filename);
        let file = fs::File::create(&path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Writer::from_writer(file))
    }

    /// Write a master and its artists to CSV.
    pub fn write_master(&mut self, master: &Master) -> Result<()> {
        let main_release = master
            .main_release_id
            .map(|id| id.to_string())
            .unwrap_or_default();
        let year = master
            .year
            .map(|y| y.to_string())
            .unwrap_or_default();

        self.master.write_record([
            &master.id.to_string(),
            &master.title,
            &main_release,
            &year,
        ])?;

        let master_id_str = master.id.to_string();
        for artist in &master.artists {
            self.master_artist.write_record([
                &master_id_str,
                &artist.id.to_string(),
                &artist.name,
            ])?;
        }

        Ok(())
    }

    /// Flush all writers.
    pub fn flush(&mut self) -> Result<()> {
        self.master.flush()?;
        self.master_artist.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master_model::*;

    #[test]
    fn test_csv_headers() {
        let dir = tempfile::tempdir().unwrap();
        let output = MasterCsvOutput::new(dir.path()).unwrap();
        drop(output);

        let mut rdr = csv::Reader::from_path(dir.path().join("master.csv")).unwrap();
        let headers: Vec<String> = rdr.headers().unwrap().iter().map(|s| s.to_string()).collect();
        assert_eq!(headers, vec!["id", "title", "main_release_id", "year"]);

        let mut rdr = csv::Reader::from_path(dir.path().join("master_artist.csv")).unwrap();
        let headers: Vec<String> = rdr.headers().unwrap().iter().map(|s| s.to_string()).collect();
        assert_eq!(headers, vec!["master_id", "artist_id", "artist_name"]);
    }

    #[test]
    fn test_write_master() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = MasterCsvOutput::new(dir.path()).unwrap();

        let master = Master {
            id: 100,
            title: "Aluminum Tunes".to_string(),
            main_release_id: Some(1001),
            year: Some(1998),
            artists: vec![MasterArtist {
                id: 3000,
                name: "Stereolab".to_string(),
            }],
        };
        output.write_master(&master).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("master.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "100");
        assert_eq!(&records[0][1], "Aluminum Tunes");
        assert_eq!(&records[0][2], "1001");
        assert_eq!(&records[0][3], "1998");
    }

    #[test]
    fn test_write_master_artists() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = MasterCsvOutput::new(dir.path()).unwrap();

        let master = Master {
            id: 300,
            title: "Duke Ellington & John Coltrane".to_string(),
            main_release_id: Some(3001),
            year: Some(1963),
            artists: vec![
                MasterArtist { id: 5000, name: "Duke Ellington".to_string() },
                MasterArtist { id: 5001, name: "John Coltrane".to_string() },
            ],
        };
        output.write_master(&master).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("master_artist.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "300");
        assert_eq!(&records[0][2], "Duke Ellington");
        assert_eq!(&records[1][2], "John Coltrane");
    }

    #[test]
    fn test_write_master_without_optionals() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = MasterCsvOutput::new(dir.path()).unwrap();

        let master = Master {
            id: 500,
            title: "Pequena Vertigem de Amor".to_string(),
            main_release_id: None,
            year: None,
            artists: vec![MasterArtist { id: 7000, name: "Sessa".to_string() }],
        };
        output.write_master(&master).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("master.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(&records[0][2], ""); // main_release_id empty
        assert_eq!(&records[0][3], ""); // year empty
    }
}
