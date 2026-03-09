//! Output trait for release data.
//!
//! Defines the `ReleaseOutput` trait that abstracts over different output
//! targets (CSV files, PostgreSQL, etc.) for the release processing pipeline.

use anyhow::Result;

use crate::model::Release;

/// Trait for writing release data to an output target.
///
/// Implementations buffer writes internally. Call `finish()` after all
/// releases have been written to flush remaining data and perform any
/// post-processing.
pub trait ReleaseOutput {
    /// Write a single release and all its child records to the output.
    fn write_release(&mut self, release: &Release) -> Result<()>;

    /// Flush any buffered data to the output target.
    fn flush(&mut self) -> Result<()>;

    /// Finalize the output: flush remaining data and perform any
    /// post-processing (e.g., artwork URL population, track count computation).
    fn finish(&mut self) -> Result<()>;
}
