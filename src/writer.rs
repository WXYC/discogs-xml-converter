//! CSV output writer for Discogs release data.
//!
//! Writes 9 CSV files matching the contract expected by `discogs-cache/scripts/import_csv.py`:
//! - release.csv
//! - release_artist.csv
//! - release_label.csv
//! - release_track.csv
//! - release_track_artist.csv
//! - release_image.csv
//! - release_genre.csv
//! - release_style.csv
//! - release_company.csv

use std::path::Path;

use anyhow::Result;
use wxyc_etl::csv_writer::{CsvFileSpec, MultiCsvWriter};

use crate::model::Release;
use crate::output::ReleaseOutput;

/// CSV file indices (matching spec order in CsvOutput::new).
const RELEASE: usize = 0;
const RELEASE_ARTIST: usize = 1;
const RELEASE_LABEL: usize = 2;
const RELEASE_TRACK: usize = 3;
const RELEASE_TRACK_ARTIST: usize = 4;
const RELEASE_IMAGE: usize = 5;
const RELEASE_GENRE: usize = 6;
const RELEASE_STYLE: usize = 7;
const RELEASE_COMPANY: usize = 8;

/// Manages 9 CSV writers via `MultiCsvWriter`, one per output file.
pub struct CsvOutput {
    csv: MultiCsvWriter,
}

impl CsvOutput {
    /// Create a new CsvOutput, writing headers to all 9 files.
    pub fn new(output_dir: &Path) -> Result<Self> {
        let specs = [
            CsvFileSpec::new(
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
            ),
            CsvFileSpec::new(
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
            ),
            CsvFileSpec::new("release_label.csv", &["release_id", "label", "catno"]),
            CsvFileSpec::new(
                "release_track.csv",
                &["release_id", "sequence", "position", "title", "duration"],
            ),
            CsvFileSpec::new(
                "release_track_artist.csv",
                &["release_id", "track_sequence", "artist_name"],
            ),
            CsvFileSpec::new(
                "release_image.csv",
                &["release_id", "type", "width", "height", "uri"],
            ),
            CsvFileSpec::new("release_genre.csv", &["release_id", "genre"]),
            CsvFileSpec::new("release_style.csv", &["release_id", "style"]),
            CsvFileSpec::new(
                "release_company.csv",
                &[
                    "release_id",
                    "company_id",
                    "company_name",
                    "entity_type",
                    "entity_type_name",
                ],
            ),
        ];

        let csv = MultiCsvWriter::new(output_dir, &specs)?;
        Ok(CsvOutput { csv })
    }

    /// Write a release and all its child records to the 9 CSV files.
    pub fn write_release(&mut self, release: &Release) -> Result<()> {
        let mut ibuf = itoa::Buffer::new();
        let id_str = ibuf.format(release.id).to_string();
        let master_id_str = release
            .master_id
            .map(|id| {
                let mut b = itoa::Buffer::new();
                b.format(id).to_string()
            })
            .unwrap_or_default();

        // release.csv
        self.csv.writer(RELEASE).write_record([
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
            let mut b = itoa::Buffer::new();
            let artist_id_str = b.format(artist.artist_id).to_string();
            let mut b2 = itoa::Buffer::new();
            let position_str = b2.format(artist.position).to_string();
            self.csv.writer(RELEASE_ARTIST).write_record([
                &id_str,
                &artist_id_str,
                &artist.name,
                "0",
                &artist.anv,
                &position_str,
                &artist.join_field,
            ])?;
        }

        // release_artist.csv - extra artists (extra=1)
        for artist in &release.extra_artists {
            let mut b = itoa::Buffer::new();
            let artist_id_str = b.format(artist.artist_id).to_string();
            let mut b2 = itoa::Buffer::new();
            let position_str = b2.format(artist.position).to_string();
            self.csv.writer(RELEASE_ARTIST).write_record([
                &id_str,
                &artist_id_str,
                &artist.name,
                "1",
                &artist.anv,
                &position_str,
                &artist.join_field,
            ])?;
        }

        // release_label.csv
        for label in &release.labels {
            self.csv
                .writer(RELEASE_LABEL)
                .write_record([&id_str, &label.name, &label.catno])?;
        }

        // release_track.csv and release_track_artist.csv
        for (idx, track) in release.tracks.iter().enumerate() {
            let mut b = itoa::Buffer::new();
            let sequence = b.format(idx + 1).to_string();

            self.csv.writer(RELEASE_TRACK).write_record([
                &id_str,
                &sequence,
                &track.position,
                &track.title,
                &track.duration,
            ])?;

            // Track artists (both main and extra go to the same table)
            for artist in &track.artists {
                self.csv
                    .writer(RELEASE_TRACK_ARTIST)
                    .write_record([&id_str, &sequence, &artist.name])?;
            }
            for artist in &track.extra_artists {
                self.csv
                    .writer(RELEASE_TRACK_ARTIST)
                    .write_record([&id_str, &sequence, &artist.name])?;
            }
        }

        // release_image.csv
        for image in &release.images {
            let mut bw = itoa::Buffer::new();
            let width_str = bw.format(image.width).to_string();
            let mut bh = itoa::Buffer::new();
            let height_str = bh.format(image.height).to_string();
            self.csv.writer(RELEASE_IMAGE).write_record([
                &id_str,
                &image.image_type,
                &width_str,
                &height_str,
                &image.uri,
            ])?;
        }

        // release_genre.csv
        for genre in &release.genres {
            self.csv
                .writer(RELEASE_GENRE)
                .write_record([&id_str, genre])?;
        }

        // release_style.csv
        for style in &release.styles {
            self.csv
                .writer(RELEASE_STYLE)
                .write_record([&id_str, style])?;
        }

        // release_company.csv
        for company in &release.companies {
            let mut b = itoa::Buffer::new();
            let company_id_str = b.format(company.company_id).to_string();
            let mut b2 = itoa::Buffer::new();
            let entity_type_str = b2.format(company.entity_type).to_string();
            self.csv.writer(RELEASE_COMPANY).write_record([
                &id_str,
                &company_id_str,
                &company.name,
                &entity_type_str,
                &company.entity_type_name,
            ])?;
        }

        Ok(())
    }

    /// Flush all writers.
    pub fn flush(&mut self) -> Result<()> {
        self.csv.flush_all()
    }

    /// Get the output directory path.
    pub fn output_dir(&self) -> &Path {
        self.csv.output_dir()
    }
}

impl ReleaseOutput for CsvOutput {
    fn write_release(&mut self, release: &Release) -> Result<()> {
        CsvOutput::write_release(self, release)
    }

    fn flush(&mut self) -> Result<()> {
        CsvOutput::flush(self)
    }

    fn finish(&mut self) -> Result<()> {
        self.flush()
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
            genres: vec!["Electronic".to_string(), "Rock".to_string()],
            styles: vec!["Alternative Rock".to_string(), "Art Rock".to_string()],
            companies: vec![ReleaseCompany {
                company_id: 271046,
                name: "The Globe Studios".to_string(),
                entity_type: 23,
                entity_type_name: "Recorded At".to_string(),
            }],
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
        check_header("release_genre.csv", &["release_id", "genre"]);
        check_header("release_style.csv", &["release_id", "style"]);
        check_header(
            "release_company.csv",
            &[
                "release_id",
                "company_id",
                "company_name",
                "entity_type",
                "entity_type_name",
            ],
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

        // Check genres
        let mut rdr = csv::Reader::from_path(dir.path().join("release_genre.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "1001");
        assert_eq!(&records[0][1], "Electronic");
        assert_eq!(&records[1][1], "Rock");

        // Check styles
        let mut rdr = csv::Reader::from_path(dir.path().join("release_style.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert_eq!(&records[0][0], "1001");
        assert_eq!(&records[0][1], "Alternative Rock");
        assert_eq!(&records[1][1], "Art Rock");

        // Check companies
        let mut rdr = csv::Reader::from_path(dir.path().join("release_company.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "1001");
        assert_eq!(&records[0][1], "271046");
        assert_eq!(&records[0][2], "The Globe Studios");
        assert_eq!(&records[0][3], "23");
        assert_eq!(&records[0][4], "Recorded At");
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
            "release_genre.csv",
            "release_style.csv",
            "release_company.csv",
        ] {
            let content1 = std::fs::read_to_string(dir1.path().join(filename)).unwrap();
            let content2 = std::fs::read_to_string(dir2.path().join(filename)).unwrap();
            assert_eq!(
                content1, content2,
                "Non-deterministic output for {}",
                filename
            );
        }
    }
}
