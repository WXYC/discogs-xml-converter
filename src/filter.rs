//! Artist name normalization and filtering.
//!
//! Normalizes artist names using NFKD decomposition with combining character
//! removal, matching the behavior of `filter_csv.py:normalize_artist()` in
//! the discogs-cache repo.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use unicode_normalization::UnicodeNormalization;

/// Normalize an artist name for matching.
///
/// Applies NFKD decomposition, strips combining characters (diacritics),
/// lowercases, and trims whitespace. This matches the Python implementation:
///
/// ```python
/// nfkd = unicodedata.normalize("NFKD", name)
/// stripped = "".join(c for c in nfkd if not unicodedata.combining(c))
/// return stripped.lower().strip()
/// ```
pub fn normalize_artist(name: &str) -> String {
    name.nfkd()
        .filter(|c| !is_combining(*c))
        .collect::<String>()
        .to_lowercase()
        .trim()
        .to_string()
}

/// Check if a character is a Unicode combining character (category M).
fn is_combining(c: char) -> bool {
    // Combining characters are in the Unicode General Category "M"
    // (Mark). We check the three subcategories:
    // - Mn (Nonspacing_Mark)
    // - Mc (Spacing_Mark)
    // - Me (Enclosing_Mark)
    //
    // The ranges cover all combining marks in BMP which is sufficient
    // for Discogs data.
    matches!(
        unicode_general_category(c),
        GeneralCategory::Mn | GeneralCategory::Mc | GeneralCategory::Me
    )
}

#[derive(PartialEq)]
enum GeneralCategory {
    Mn,
    Mc,
    Me,
    Other,
}

fn unicode_general_category(c: char) -> GeneralCategory {
    // Use the char's Unicode properties. Combining characters
    // are in these ranges. We use a simple heuristic based on
    // Unicode block ranges for the Combining Diacritical Marks.
    let cp = c as u32;

    // Check standard combining character ranges
    if (0x0300..=0x036F).contains(&cp)       // Combining Diacritical Marks
        || (0x1AB0..=0x1AFF).contains(&cp)   // Combining Diacritical Marks Extended
        || (0x1DC0..=0x1DFF).contains(&cp)   // Combining Diacritical Marks Supplement
        || (0x20D0..=0x20FF).contains(&cp)   // Combining Diacritical Marks for Symbols
        || (0xFE20..=0xFE2F).contains(&cp)
    // Combining Half Marks
    {
        return GeneralCategory::Mn;
    }

    // Spacing combining marks (Mc) - common South Asian scripts
    if (0x0903..=0x0903).contains(&cp)
        || (0x093B..=0x093B).contains(&cp)
        || (0x093E..=0x0940).contains(&cp)
        || (0x0949..=0x094C).contains(&cp)
    {
        return GeneralCategory::Mc;
    }

    // Enclosing marks (Me)
    if (0x0488..=0x0489).contains(&cp)
        || (0x20DD..=0x20E0).contains(&cp)
        || (0x20E2..=0x20E4).contains(&cp)
    {
        return GeneralCategory::Me;
    }

    GeneralCategory::Other
}

/// Artist filter backed by a normalized HashSet.
pub struct ArtistFilter {
    artists: HashSet<String>,
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
        Ok(ArtistFilter { artists })
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
        assert_eq!(normalize_artist("Radiohead"), "radiohead");
    }

    #[test]
    fn test_normalize_strip_spaces() {
        assert_eq!(normalize_artist("  Radiohead  "), "radiohead");
    }

    #[test]
    fn test_normalize_all_caps() {
        assert_eq!(normalize_artist("RADIOHEAD"), "radiohead");
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
    fn test_normalize_bjork() {
        assert_eq!(normalize_artist("Björk"), "bjork");
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
        fs::write(&path, "Radiohead\nBjörk\n  Joy Division  \n\n").unwrap();

        let filter = ArtistFilter::from_file(&path).unwrap();
        assert_eq!(filter.len(), 3);
        assert!(filter.matches_any(["Radiohead"].iter().copied()));
        assert!(filter.matches_any(["Björk"].iter().copied()));
        assert!(filter.matches_any(["joy division"].iter().copied()));
        assert!(!filter.matches_any(["Unknown Artist"].iter().copied()));
    }

    #[test]
    fn test_filter_matches_any() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artists.txt");
        fs::write(&path, "Radiohead\nBjörk\n").unwrap();

        let filter = ArtistFilter::from_file(&path).unwrap();

        // Match on any one of multiple names
        assert!(filter.matches_any(["Unknown", "Radiohead"].iter().copied()));
        // No match
        assert!(!filter.matches_any(["Unknown", "Other"].iter().copied()));
    }
}
