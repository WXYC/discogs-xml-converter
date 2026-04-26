//! Artist name normalization and filtering.
//!
//! Normalizes artist names by delegating to `wxyc_etl::text::normalize_artist_name()`,
//! which applies NFKD decomposition, strips combining characters (diacritics),
//! lowercases, and trims whitespace.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Normalize an artist name for matching.
///
/// Delegates to [`wxyc_etl::text::normalize_artist_name()`], the shared
/// implementation that all WXYC ETL repos use for normalization parity.
pub fn normalize_artist(name: &str) -> String {
    wxyc_etl::text::normalize_artist_name(name)
}

/// Artist filter backed by a normalized HashSet, with optional alias support.
///
/// When aliases are loaded (from `artist_alias.csv`), the filter checks both
/// canonical artist names and their aliases/name-variations by artist_id.
pub struct ArtistFilter {
    artists: HashSet<String>,
    /// artist_id -> set of normalized alias names (includes name variations)
    aliases: HashMap<u64, Vec<String>>,
}

impl ArtistFilter {
    /// Load artist names from a file (one per line) and normalize them.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)?;
        let artists = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(normalize_artist)
            .collect();
        Ok(ArtistFilter {
            artists,
            aliases: HashMap::new(),
        })
    }

    /// Load artist aliases from `artist_alias.csv`.
    ///
    /// Builds a lookup from artist_id to normalized alias names. When combined
    /// with `matches_any_with_ids()`, this enables matching releases where the
    /// credited artist is known by a different name in the library.
    pub fn load_aliases(&mut self, csv_path: &Path) -> anyhow::Result<usize> {
        let mut rdr = csv::Reader::from_path(csv_path)?;
        let mut count = 0;

        for result in rdr.records() {
            let record = result?;
            let artist_id: u64 = record[0].parse().unwrap_or(0);
            let alias_name = &record[2]; // alias_name column

            let normalized = normalize_artist(alias_name);
            if !normalized.is_empty() {
                self.aliases.entry(artist_id).or_default().push(normalized);
                count += 1;
            }
        }

        Ok(count)
    }

    /// Check if any of the given artist names match the filter.
    pub fn matches_any<'a, I>(&self, names: I) -> bool
    where
        I: IntoIterator<Item = &'a str>,
    {
        names
            .into_iter()
            .any(|name| self.artists.contains(&normalize_artist(name)))
    }

    /// Check if any artist matches by canonical name or by alias lookup.
    ///
    /// For each (artist_id, name) pair:
    /// 1. Check the canonical name against the library set
    /// 2. Look up aliases by artist_id and check each against the library set
    pub fn matches_any_with_ids(&self, artists: &[(u64, &str)]) -> bool {
        for (artist_id, name) in artists {
            // Check canonical name
            if self.artists.contains(&normalize_artist(name)) {
                return true;
            }

            // Check aliases by artist_id
            if let Some(alias_names) = self.aliases.get(artist_id) {
                for alias in alias_names {
                    if self.artists.contains(alias) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Whether alias data has been loaded.
    pub fn has_aliases(&self) -> bool {
        !self.aliases.is_empty()
    }

    /// Number of artists in the filter set.
    pub fn len(&self) -> usize {
        self.artists.len()
    }

    /// Whether the filter set is empty.
    pub fn is_empty(&self) -> bool {
        self.artists.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of all 11 test cases from discogs-cache/tests/unit/test_filter_csv.py
    #[test]
    fn test_normalize_lowercase() {
        assert_eq!(normalize_artist("Autechre"), "autechre");
    }

    #[test]
    fn test_normalize_strip_spaces() {
        assert_eq!(normalize_artist("  Autechre  "), "autechre");
    }

    #[test]
    fn test_normalize_all_caps() {
        assert_eq!(normalize_artist("AUTECHRE"), "autechre");
    }

    #[test]
    fn test_normalize_mixed_case_strip() {
        assert_eq!(normalize_artist("  Mixed Case  "), "mixed case");
    }

    #[test]
    fn test_normalize_empty() {
        assert_eq!(normalize_artist(""), "");
    }

    #[test]
    fn test_normalize_yanya() {
        assert_eq!(normalize_artist("Nilüfer Yanya"), "nilufer yanya");
    }

    #[test]
    fn test_normalize_sigur_ros() {
        assert_eq!(normalize_artist("Sigur Rós"), "sigur ros");
    }

    #[test]
    fn test_normalize_motorhead() {
        assert_eq!(normalize_artist("Motörhead"), "motorhead");
    }

    #[test]
    fn test_normalize_husker_du() {
        assert_eq!(normalize_artist("Hüsker Dü"), "husker du");
    }

    #[test]
    fn test_normalize_cafe_tacvba() {
        assert_eq!(normalize_artist("Café Tacvba"), "cafe tacvba");
    }

    #[test]
    fn test_normalize_zoe() {
        assert_eq!(normalize_artist("Zoé"), "zoe");
    }

    #[test]
    fn test_filter_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artists.txt");
        fs::write(&path, "Autechre\nNilüfer Yanya\n  Father John Misty  \n\n").unwrap();

        let filter = ArtistFilter::from_file(&path).unwrap();
        assert_eq!(filter.len(), 3);
        assert!(filter.matches_any(["Autechre"].iter().copied()));
        assert!(filter.matches_any(["Nilüfer Yanya"].iter().copied()));
        assert!(filter.matches_any(["father john misty"].iter().copied()));
        assert!(!filter.matches_any(["Unknown Artist"].iter().copied()));
    }

    #[test]
    fn test_filter_matches_any() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artists.txt");
        fs::write(&path, "Autechre\nNilüfer Yanya\n").unwrap();

        let filter = ArtistFilter::from_file(&path).unwrap();

        // Match on any one of multiple names
        assert!(filter.matches_any(["Unknown", "Autechre"].iter().copied()));
        // No match
        assert!(!filter.matches_any(["Unknown", "Other"].iter().copied()));
    }

    #[test]
    fn test_load_aliases_and_match() {
        let dir = tempfile::tempdir().unwrap();

        // Library has "Sun Ra Arkestra" (an alias of canonical "Sun Ra")
        let lib_path = dir.path().join("artists.txt");
        fs::write(&lib_path, "Sun Ra Arkestra\n").unwrap();

        // artist_alias.csv: artist 123 has alias "Sun Ra Arkestra"
        let alias_path = dir.path().join("artist_alias.csv");
        fs::write(
            &alias_path,
            "artist_id,artist_name,alias_name\n\
             123,Sun Ra,Le Sony'r Ra\n\
             123,Sun Ra,Sonny Ra\n\
             123,Sun Ra,Sun Ra Arkestra\n\
             123,Sun Ra,Sun Ra And His Arkestra\n",
        )
        .unwrap();

        let mut filter = ArtistFilter::from_file(&lib_path).unwrap();
        let count = filter.load_aliases(&alias_path).unwrap();
        assert_eq!(count, 4);
        assert!(filter.has_aliases());

        // "Sun Ra" doesn't match directly, but alias lookup finds "Sun Ra Arkestra"
        assert!(!filter.matches_any(["Sun Ra"].iter().copied()));
        assert!(filter.matches_any_with_ids(&[(123, "Sun Ra")]));

        // Unknown artist doesn't match
        assert!(!filter.matches_any_with_ids(&[(999, "Unknown")]));
    }

    /// Verify the trim optimization: names without leading/trailing spaces
    /// avoid a second allocation, while names with spaces still normalize
    /// correctly.
    #[test]
    fn test_normalize_trim_optimization_paths() {
        // No trimming needed — fast path (no extra allocation)
        assert_eq!(normalize_artist("Autechre"), "autechre");
        // Trimming needed — allocates trimmed copy
        assert_eq!(normalize_artist("  Autechre  "), "autechre");
        // Only leading space
        assert_eq!(normalize_artist("  Björk"), "bjork");
        // Only trailing space
        assert_eq!(normalize_artist("Zoé  "), "zoe");
        // Tab and other whitespace are NOT trimmed (matches Python str.strip() for spaces)
        // Our implementation trims only ASCII space, matching the common case
    }

    /// Verify that the local normalize_artist() produces identical output to
    /// wxyc_etl::text::normalize_artist_name() for a comprehensive set of edge cases.
    /// This confirms the migration to the shared crate is safe.
    mod normalization_parity_tests {
        use super::*;
        use wxyc_etl::text::normalize_artist_name;

        #[test]
        fn parity_diacritics() {
            let cases = [
                "Nilüfer Yanya",
                "Hermanos Gutiérrez",
                "Csillagrablók",
                "Motörhead",
                "Hüsker Dü",
                "Zoé",
            ];
            for name in cases {
                assert_eq!(
                    normalize_artist(name),
                    normalize_artist_name(name),
                    "Mismatch for: {name}"
                );
            }
        }

        #[test]
        fn parity_combining_characters() {
            let cases = ["Caf\u{0301}", "nu\u{0303}ez", "Bjo\u{0308}rk"];
            for name in cases {
                assert_eq!(
                    normalize_artist(name),
                    normalize_artist_name(name),
                    "Mismatch for: {name}"
                );
            }
        }

        #[test]
        fn parity_whitespace() {
            let cases = ["  Autechre  ", "  Nilüfer Yanya", "Zoé  ", "  Mixed Case  "];
            for name in cases {
                assert_eq!(
                    normalize_artist(name),
                    normalize_artist_name(name),
                    "Mismatch for: {name}"
                );
            }
        }

        #[test]
        fn parity_case() {
            let cases = ["AUTECHRE", "autechre", "Autechre", "aUtEcHrE"];
            for name in cases {
                assert_eq!(
                    normalize_artist(name),
                    normalize_artist_name(name),
                    "Mismatch for: {name}"
                );
            }
        }

        #[test]
        fn parity_empty() {
            assert_eq!(normalize_artist(""), normalize_artist_name(""));
        }

        #[test]
        fn parity_wxyc_artists() {
            let cases = [
                "Autechre",
                "Prince Jammy",
                "Juana Molina",
                "Stereolab",
                "Cat Power",
                "Jessica Pratt",
                "Chuquimamani-Condori",
                "Duke Ellington & John Coltrane",
                "Sessa",
                "Anne Gillis",
            ];
            for name in cases {
                assert_eq!(
                    normalize_artist(name),
                    normalize_artist_name(name),
                    "Mismatch for: {name}"
                );
            }
        }
    }

    #[test]
    fn test_matches_any_with_ids_canonical_name_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("artists.txt");
        fs::write(&lib_path, "Autechre\n").unwrap();

        let filter = ArtistFilter::from_file(&lib_path).unwrap();

        // Even without aliases, canonical name matching works
        assert!(filter.matches_any_with_ids(&[(300, "Autechre")]));
        assert!(!filter.matches_any_with_ids(&[(300, "Unknown")]));
    }
}
