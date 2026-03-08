//! CSV output writer for Discogs label hierarchy data.
//!
//! Writes label_hierarchy.csv with parent-child label relationships.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use csv::Writer;

use crate::label_model::Label;

/// Manages CSV writer for label hierarchy output.
pub struct LabelCsvOutput {
    hierarchy: Writer<fs::File>,
}

impl LabelCsvOutput {
    /// Create a new LabelCsvOutput, writing headers.
    pub fn new(output_dir: &Path) -> Result<Self> {
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "Failed to create output directory: {}",
                output_dir.display()
            )
        })?;

        let mut hierarchy = Self::create_writer(output_dir, "label_hierarchy.csv")?;
        hierarchy.write_record([
            "label_id",
            "label_name",
            "parent_label_id",
            "parent_label_name",
        ])?;

        Ok(LabelCsvOutput { hierarchy })
    }

    fn create_writer(dir: &Path, filename: &str) -> Result<Writer<fs::File>> {
        let path = dir.join(filename);
        let file = fs::File::create(&path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        Ok(Writer::from_writer(file))
    }

    /// Write a label's parent relationship to CSV (only if it has a parent).
    pub fn write_label(&mut self, label: &Label) -> Result<()> {
        if let Some(parent_id) = label.parent_id {
            self.hierarchy.write_record([
                &label.id.to_string(),
                &label.name,
                &parent_id.to_string(),
                &label.parent_name,
            ])?;
        }
        Ok(())
    }

    /// Flush all writers.
    pub fn flush(&mut self) -> Result<()> {
        self.hierarchy.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label_model::*;

    #[test]
    fn test_csv_headers() {
        let dir = tempfile::tempdir().unwrap();
        let output = LabelCsvOutput::new(dir.path()).unwrap();
        drop(output);

        let mut rdr = csv::Reader::from_path(dir.path().join("label_hierarchy.csv")).unwrap();
        let headers: Vec<String> = rdr
            .headers()
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            headers,
            vec![
                "label_id",
                "label_name",
                "parent_label_id",
                "parent_label_name"
            ]
        );
    }

    #[test]
    fn test_write_label_with_parent() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = LabelCsvOutput::new(dir.path()).unwrap();

        let label = Label {
            id: 2,
            name: "Parlophone".to_string(),
            parent_id: Some(1),
            parent_name: "EMI".to_string(),
        };
        output.write_label(&label).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("label_hierarchy.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0][0], "2");
        assert_eq!(&records[0][1], "Parlophone");
        assert_eq!(&records[0][2], "1");
        assert_eq!(&records[0][3], "EMI");
    }

    #[test]
    fn test_write_label_without_parent() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = LabelCsvOutput::new(dir.path()).unwrap();

        let label = Label {
            id: 4,
            name: "Sub Pop".to_string(),
            parent_id: None,
            parent_name: "".to_string(),
        };
        output.write_label(&label).unwrap();
        output.flush().unwrap();

        let mut rdr = csv::Reader::from_path(dir.path().join("label_hierarchy.csv")).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(
            records.len(),
            0,
            "Labels without parents should not be written"
        );
    }
}
