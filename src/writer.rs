//! CSV output writer for Discogs release data.
//!
//! Writes 6 CSV files matching the contract expected by `discogs-cache/scripts/import_csv.py`:
//! - release.csv
//! - release_artist.csv
//! - release_label.csv
//! - release_track.csv
//! - release_track_artist.csv
//! - release_image.csv

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use csv::Writer;

use crate::model::Release;

/// Manages 6 CSV writers, one per output file.
pub struct CsvOutput {
    release: Writer<fs::File>,
    release_artist: Writer<fs::File>,
    release_label: Writer<fs::File>,
    release_track: Writer<fs::File>,
    release_track_artist: Writer<fs::File>,
    release_image: Writer<fs::File>,
    output_dir: PathBuf,
}

impl CsvOutput {
    /// Create a new CsvOutput, writing headers to all 6 files.
    pub fn new(output_dir: &Path) -> Result<Self> {
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        let mut release = Self::create_writer(output_dir, "release.csv")?;
        release.write_record([
            "id",
            "status",
            "title",
            "country",
            "released",
            "notes",
            "data_quality",
            "master_id",
            "format",
        ])?;

        let mut release_artist = Self::create_writer(output_dir, "release_artist.csv")?;
        release_artist.write_record([
            "release_id",
            "artist_id",
            "artist_name",
            "extra",
            "anv",
            "position",
            "join_field",
        ])?;

        let mut release_label = Self::create_writer(output_dir, "release_label.csv")?;
        release_label.write_record(["release_id", "label", "catno"])?;

        let mut release_track = Self::create_writer(output_dir, "release_track.csv")?;
        release_track.write_record(["release_id", "sequence", "position", "title", "duration"])?;

        let mut release_track_artist = Self::create_writer(output_dir, "release_track_artist.csv")?;
        release_track_artist.write_record(["release_id", "track_sequence", "artist_name"])?;

        let mut release_image = Self::create_writer(output_dir, "release_image.csv")?;
        release_image.write_record(["release_id", "type", "width", "height", "uri"])?;

        Ok(CsvOutput {
            release,
            release_artist,
            release_label,
            release_track,
            release_track_artist,
            release_image,
            output_dir: output_dir.to_path_buf(),
        })
    }

    fn create_writer(dir: &Path, filename: &str) -> Result<Writer<fs::File>> {
        let path = dir.join(filename);
        let file = fs::File::create(&path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Writer::from_writer(file))
    }

    /// Write a release and all its child records to the 6 CSV files.
    pub fn write_release(&mut self, release: &Release) -> Result<()> {
        let id_str = release.id.to_string();
        let master_id_str = release
            .master_id
            .map(|id| id.to_string())
            .unwrap_or_default();

        // release.csv
        self.release.write_record([
            &id_str,
            &release.status,
            &release.title,
            &release.country,
            &release.released,
            &release.notes,
            &release.data_quality,
            &master_id_str,
            &release.format_string(),
        ])?;

        // release_artist.csv - main artists (extra=0)
        for artist in &release.artists {
            self.release_artist.write_record([
                &id_str,
                &artist.artist_id.to_string(),
                &artist.name,
                "0",
                &artist.anv,
                &artist.position.to_string(),
                &artist.join_field,
            ])?;
        }

        // release_artist.csv - extra artists (extra=1)
        for artist in &release.extra_artists {
            self.release_artist.write_record([
                &id_str,
                &artist.artist_id.to_string(),
                &artist.name,
                "1",
                &artist.anv,
                &artist.position.to_string(),
                &artist.join_field,
            ])?;
        }

        // release_label.csv
        for label in &release.labels {
            self.release_label
                .write_record([&id_str, &label.name, &label.catno])?;
        }

        // release_track.csv and release_track_artist.csv
        for (idx, track) in release.tracks.iter().enumerate() {
            let sequence = (idx + 1).to_string();

            self.release_track.write_record([
                &id_str,
                &sequence,
                &track.position,
                &track.title,
                &track.duration,
            ])?;

            // Track artists (both main and extra go to the same table)
            for artist in &track.artists {
                self.release_track_artist
                    .write_record([&id_str, &sequence, &artist.name])?;
            }
            for artist in &track.extra_artists {
                self.release_track_artist
                    .write_record([&id_str, &sequence, &artist.name])?;
            }
        }

        // release_image.csv
        for image in &release.images {
            self.release_image.write_record([
                &id_str,
                &image.image_type,
                &image.width.to_string(),
                &image.height.to_string(),
                &image.uri,
            ])?;
        }

        Ok(())
    }

    /// Flush all writers.
    pub fn flush(&mut self) -> Result<()> {
        self.release.flush()?;
        self.release_artist.flush()?;
        self.release_label.flush()?;
        self.release_track.flush()?;
        self.release_track_artist.flush()?;
        self.release_image.flush()?;
        Ok(())
    }

    /// Get the output directory path.
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn sample_release() -> Release {
        Release {
            id: 1001,
            status: "Accepted".to_string(),
            title: "OK Computer".to_string(),
            country: "UK".to_string(),
            released: "1997-06-16".to_string(),
            notes: "".to_string(),
            data_quality: "Correct".to_string(),
            master_id: Some(500),
            formats: vec![Format {
                name: "CD".to_string(),
                qty: 1,
            }],
            artists: vec![ReleaseArtist {
                artist_id: 1,
                name: "Radiohead".to_string(),
                anv: "".to_string(),
                join_field: "".to_string(),
                position: 1,
            }],
            extra_artists: vec![ReleaseArtist {
                artist_id: 12,
                name: "Some Producer".to_string(),
                anv: "".to_string(),
                join_field: "".to_string(),
                position: 2,
            }],
            labels: vec![
                ReleaseLabel {
                    name: "Parlophone".to_string(),
                    catno: "7243 8 55229 2 8".to_string(),
                },
                ReleaseLabel {
                    name: "Capitol Records".to_string(),
                    catno: "CDP 7243 8 55229 2 8".to_string(),
                },
            ],
            tracks: vec![
                ReleaseTrack {
                    position: "1".to_string(),
                    title: "Airbag".to_string(),
                    duration: "4:44".to_string(),
                    artists: vec![],
                    extra_artists: vec![],
                },
                ReleaseTrack {
                    position: "2".to_string(),
                    title: "Paranoid Android".to_string(),
                    duration: "6:23".to_string(),
                    artists: vec![],
                    extra_artists: vec![],
                },
                ReleaseTrack {
                    position: "3".to_string(),
                    title: "Subterranean Homesick Alien".to_string(),
                    duration: "4:27".to_string(),
                    artists: vec![],
                    extra_artists: vec![],
                },
            ],
            images: vec![
                ReleaseImage {
                    image_type: "primary".to_string(),
                    width: 600,
                    height: 600,
                    uri: "https://img.discogs.com/abc123/release-1001.jpg".to_string(),
                },
                ReleaseImage {
                    image_type: "secondary".to_string(),
                    width: 300,
                    height: 300,
                    uri: "https://img.discogs.com/abc123/release-1001-back.jpg".to_string(),
                },
            ],
        }
    }

    #[test]
    fn test_csv_headers() {
        let dir = tempfile::tempdir().unwrap();
        let output = CsvOutput::new(dir.path()).unwrap();
        drop(output);

        // Read back headers from each file
        let check_header = |filename: &str, expected: &[&str]| {
            let path = dir.path().join(filename);
            let mut rdr = csv::Reader::from_path(&path).unwrap();
            let headers: Vec<String> = rdr
                .headers()
                .unwrap()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            assert_eq!(headers, expected, "Headers mismatch for {}", filename);
        };

        check_header(
            "release.csv",
            &[
                "id",
                "status",
                "title",
                "country",
                "released",
                "notes",
                "data_quality",
                "master_id",
                "format",
            ],
        );
        check_header(
            "release_artist.csv",
            &[
                "release_id",
                "artist_id",
                "artist_name",
                "extra",
                "anv",
                "position",
                "join_field",
            ],
        );
        check_header("release_label.csv", &["release_id", "label", "catno"]);
        check_header(
            "release_track.csv",
            &["release_id", "sequence", "position", "title", "duration"],
        );
        check_header(
            "release_track_artist.csv",
            &["release_id", "track_sequence", "artist_name"],
        );
        check_header(
            "release_image.csv",
            &["release_id", "type", "width", "height", "uri"],
        );
    }

    #[test]
    fn test_write_release_basic() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = CsvOutput::new(dir.path()).unwrap();
        output.write_release(&sample_release()).unwrap();
        output.flush().unwrap();

        // Check release.csv
        let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "1001");
        assert_eq!(&records[0][2], "OK Computer");
        assert_eq!(&records[0][7], "500");
        assert_eq!(&records[0][8], "CD");

        // Check release_artist.csv (1 main + 1 extra = 2)
        let mut rdr = csv::Reader::from_path(dir.path().join("release_artist.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][2], "Radiohead");
        assert_eq!(&records[0][3], "0"); // extra=0
        assert_eq!(&records[1][2], "Some Producer");
        assert_eq!(&records[1][3], "1"); // extra=1

        // Check tracks
        let mut rdr = csv::Reader::from_path(dir.path().join("release_track.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 3);
        assert_eq!(&records[0][1], "1"); // sequence
        assert_eq!(&records[0][3], "Airbag");
    }

    #[test]
    fn test_rfc4180_quoting() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = CsvOutput::new(dir.path()).unwrap();

        let release = Release {
            id: 9999,
            status: "Accepted".to_string(),
            title: "Title with \"quotes\"".to_string(),
            notes: "Line 1\nLine 2".to_string(),
            artists: vec![ReleaseArtist {
                artist_id: 1,
                name: "Beatles, The".to_string(),
                position: 1,
                ..Default::default()
            }],
            ..Default::default()
        };

        output.write_release(&release).unwrap();
        output.flush().unwrap();

        // Read back and verify the values survived round-trip
        let mut rdr = csv::Reader::from_path(dir.path().join("release.csv")).unwrap();
        let record = rdr.records().next().unwrap().unwrap();
        assert_eq!(&record[2], "Title with \"quotes\"");
        assert_eq!(&record[5], "Line 1\nLine 2");

        let mut rdr = csv::Reader::from_path(dir.path().join("release_artist.csv")).unwrap();
        let record = rdr.records().next().unwrap().unwrap();
        assert_eq!(&record[2], "Beatles, The");
    }

    #[test]
    fn test_track_artists_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = CsvOutput::new(dir.path()).unwrap();

        let release = Release {
            id: 8001,
            artists: vec![ReleaseArtist {
                artist_id: 7,
                name: "Various".to_string(),
                position: 1,
                ..Default::default()
            }],
            tracks: vec![
                ReleaseTrack {
                    position: "A1".to_string(),
                    title: "Rapper's Delight".to_string(),
                    duration: "14:35".to_string(),
                    artists: vec![TrackArtist {
                        name: "Sugarhill Gang".to_string(),
                    }],
                    extra_artists: vec![],
                },
                ReleaseTrack {
                    position: "A2".to_string(),
                    title: "Apache".to_string(),
                    duration: "5:35".to_string(),
                    artists: vec![TrackArtist {
                        name: "Incredible Bongo Band".to_string(),
                    }],
                    extra_artists: vec![],
                },
            ],
            ..Default::default()
        };

        output.write_release(&release).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("release_track_artist.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "8001"); // release_id
        assert_eq!(&records[0][1], "1"); // track_sequence
        assert_eq!(&records[0][2], "Sugarhill Gang");
        assert_eq!(&records[1][1], "2");
        assert_eq!(&records[1][2], "Incredible Bongo Band");
    }

    #[test]
    fn test_determinism() {
        let release = sample_release();

        let dir1 = tempfile::tempdir().unwrap();
        let mut out1 = CsvOutput::new(dir1.path()).unwrap();
        out1.write_release(&release).unwrap();
        out1.flush().unwrap();

        let dir2 = tempfile::tempdir().unwrap();
        let mut out2 = CsvOutput::new(dir2.path()).unwrap();
        out2.write_release(&release).unwrap();
        out2.flush().unwrap();

        for filename in &[
            "release.csv",
            "release_artist.csv",
            "release_label.csv",
            "release_track.csv",
            "release_track_artist.csv",
            "release_image.csv",
        ] {
            let content1 = fs::read_to_string(dir1.path().join(filename)).unwrap();
            let content2 = fs::read_to_string(dir2.path().join(filename)).unwrap();
            assert_eq!(
                content1, content2,
                "Non-deterministic output for {}",
                filename
            );
        }
    }
}
