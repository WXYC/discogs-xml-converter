//! Parser for the `--keep-release-ids` allowlist file.
//!
//! Consumed alongside the `(artist, title)` / artist-only filter (see
//! `library_pairs.rs` / `filter.rs`): any release whose id appears in this
//! allowlist is emitted even if it fails that filter. This exempts WXYC
//! library-pinned overrides (`lml_cache.library_release_override` on the
//! discogs-etl side) whose credited artist falls outside the library-artist
//! scope the monthly rebuild otherwise filters to.
//!
//! File format: plain text, one Discogs `release_id` (integer) per line.
//! Blank lines and lines starting with `#` are ignored. No header, no CSV
//! quoting/escaping. This mirrors `lib/keep_release_ids.py` in discogs-etl
//! (`parse_keep_release_ids`), which is the sole writer of this file.
//!
//! A malformed non-blank, non-comment line is a hard error rather than a
//! skip. The discogs-etl reference implementation parses each line with
//! Python's `int(line)`, which raises on anything that isn't a valid
//! integer -- so a corrupt allowlist already fails loud on that side. This
//! module keeps that behavior on read: an allowlist is a data contract
//! (discogs-etl's writer only ever emits validated integers, comments, and
//! blank lines), and silently dropping a malformed line would risk quietly
//! shrinking the exemption set for a release someone deliberately pinned.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Parse a newline-separated release_id allowlist file.
///
/// Returns an empty set if `path` does not exist, so callers can treat "no
/// override file" and "empty override file" identically -- matching the
/// discogs-etl writer, which degrades to an absent/empty file when its
/// source table isn't present.
pub fn parse_keep_release_ids(path: &Path) -> Result<HashSet<u64>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read keep-release-ids file {}", path.display()))?;

    let mut ids = HashSet::new();
    for (line_no, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let id: u64 = line.parse().with_context(|| {
            format!(
                "Malformed release id on line {} of {}: {:?}",
                line_no + 1,
                path.display(),
                raw_line
            )
        })?;
        ids.insert(id);
    }

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_empty_set_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.txt");

        let ids = parse_keep_release_ids(&path).unwrap();

        assert!(ids.is_empty());
    }

    #[test]
    fn parses_one_id_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(&path, "123\n456\n789\n").unwrap();

        let ids = parse_keep_release_ids(&path).unwrap();

        assert_eq!(ids, HashSet::from([123, 456, 789]));
    }

    #[test]
    fn ignores_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(&path, "123\n\n\n456\n").unwrap();

        let ids = parse_keep_release_ids(&path).unwrap();

        assert_eq!(ids, HashSet::from([123, 456]));
    }

    #[test]
    fn ignores_comment_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(
            &path,
            "# WXYC library-pinned overrides\n123\n# another comment\n456\n",
        )
        .unwrap();

        let ids = parse_keep_release_ids(&path).unwrap();

        assert_eq!(ids, HashSet::from([123, 456]));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(&path, "  123  \n\t456\t\n").unwrap();

        let ids = parse_keep_release_ids(&path).unwrap();

        assert_eq!(ids, HashSet::from([123, 456]));
    }

    #[test]
    fn empty_file_returns_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(&path, "").unwrap();

        let ids = parse_keep_release_ids(&path).unwrap();

        assert!(ids.is_empty());
    }

    #[test]
    fn malformed_line_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(&path, "123\nnot-a-number\n456\n").unwrap();

        let err = parse_keep_release_ids(&path).unwrap_err();

        let msg = format!("{err:#}");
        assert!(
            msg.contains("not-a-number"),
            "error should surface the offending line, got: {msg}"
        );
    }

    #[test]
    fn negative_number_is_a_hard_error() {
        // release_id is unsigned; a negative value is a data-contract
        // violation, not a valid id to silently coerce or drop.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keep_release_ids.txt");
        fs::write(&path, "-1\n").unwrap();

        assert!(parse_keep_release_ids(&path).is_err());
    }
}
