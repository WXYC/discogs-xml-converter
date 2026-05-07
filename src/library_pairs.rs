//! Pair-wise (artist, title) filter loaded from a SQLite `library.db`.
//!
//! Used by the monthly Discogs cache rebuild on EC2. The artist-only filter
//! still keeps ~4M releases — the rebuild's destination Postgres can't hold
//! that volume. Narrowing to releases whose `(artist, title)` pair appears
//! in the WXYC library brings it to ~50K.
//!
//! Data layout: an inverted index `{normalized_title -> set<normalized_artist>}`
//! so the streaming scanner does one lookup per candidate release keyed by
//! title, then a small set probe per credited artist. Mirrors the Python
//! reference at `discogs-etl/scripts/filter_csv.py::load_library_pairs`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};

use crate::filter::normalize_artist;

/// Normalize a release title for pair matching.
///
/// Same shape as [`crate::filter::normalize_artist`] (NFKD + strip combining
/// characters + lowercase + trim) so a Discogs title with diacritics matches
/// a library title without them, and vice versa. Delegates to
/// [`wxyc_etl::text::to_match_form`] for parity with every other WXYC tool.
pub fn normalize_title(title: &str) -> String {
    wxyc_etl::text::to_match_form(title)
}

/// Inverted-index of WXYC library `(artist, title)` pairs.
#[derive(Debug)]
pub struct LibraryPairs {
    pairs: HashMap<String, HashSet<String>>,
}

impl LibraryPairs {
    /// Load every `(artist, title)` row from `library.db`'s `library` table
    /// into the inverted index. Empty `artist` or `title` rows are skipped
    /// (matches the Python reference); duplicates collapse via the set.
    pub fn from_db(path: &Path) -> Result<Self> {
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("Failed to open library.db at {}", path.display()))?;

        let mut stmt = conn
            .prepare("SELECT artist, title FROM library")
            .context("library.db is missing the expected `library(artist, title)` table")?;

        let mut pairs: HashMap<String, HashSet<String>> = HashMap::new();
        let rows = stmt.query_map([], |row| {
            let artist: String = row.get(0)?;
            let title: String = row.get(1)?;
            Ok((artist, title))
        })?;

        for row in rows {
            let (artist, title) = row?;
            if artist.is_empty() || title.is_empty() {
                continue;
            }
            let n_title = normalize_title(&title);
            let n_artist = normalize_artist(&artist);
            if n_title.is_empty() || n_artist.is_empty() {
                continue;
            }
            pairs.entry(n_title).or_default().insert(n_artist);
        }

        Ok(LibraryPairs { pairs })
    }

    /// Check whether `title` paired with any of `artist_names` appears in the
    /// library. Both sides are normalized before comparison; the inputs may
    /// be the raw values pulled from a Discogs `<release>`.
    pub fn matches<'a, I>(&self, title: &str, artist_names: I) -> bool
    where
        I: IntoIterator<Item = &'a str>,
    {
        let n_title = normalize_title(title);
        let library_artists = match self.pairs.get(&n_title) {
            Some(set) => set,
            None => return false,
        };
        artist_names
            .into_iter()
            .any(|name| library_artists.contains(&normalize_artist(name)))
    }

    /// Number of distinct normalized titles in the index.
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Total `(title, artist)` pairs spanned by the index. Diagnostic only.
    pub fn pair_count(&self) -> usize {
        self.pairs.values().map(|s| s.len()).sum()
    }

    /// Look up the artist set for a normalized title. Test-only convenience.
    #[cfg(test)]
    pub fn artists_for_title(&self, normalized_title: &str) -> Option<&HashSet<String>> {
        self.pairs.get(normalized_title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn make_library_db(path: &Path, rows: &[(&str, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE library (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                artist TEXT NOT NULL,\
                title TEXT NOT NULL,\
                format TEXT\
             );",
        )
        .unwrap();
        let mut stmt = conn
            .prepare("INSERT INTO library (artist, title) VALUES (?1, ?2)")
            .unwrap();
        for (artist, title) in rows {
            stmt.execute(rusqlite::params![artist, title]).unwrap();
        }
    }

    // ---- normalize_title -------------------------------------------------

    #[test]
    fn normalize_title_lowercase() {
        assert_eq!(normalize_title("Confield"), "confield");
    }

    #[test]
    fn normalize_title_trims_spaces() {
        assert_eq!(normalize_title("  Confield  "), "confield");
    }

    #[test]
    fn normalize_title_all_caps() {
        assert_eq!(normalize_title("CONFIELD"), "confield");
    }

    #[test]
    fn normalize_title_empty() {
        assert_eq!(normalize_title(""), "");
    }

    #[test]
    fn normalize_title_diacritics() {
        assert_eq!(
            normalize_title("Pequeña Vertigem de Amor"),
            "pequena vertigem de amor"
        );
        assert_eq!(
            normalize_title("Père Ubu's Métal Box"),
            "pere ubu's metal box"
        );
    }

    /// The whole point of the pair-filter is that titles normalize identically
    /// to artist names so a diacritic-mismatched library and Discogs row still
    /// collide. Pin that.
    #[test]
    fn normalize_title_parity_with_artist() {
        let cases = [
            "PAINLESS",
            "Métal Box",
            "  On Your Own Love Again  ",
            "Pequeña Vertigem de Amor",
        ];
        for s in cases {
            assert_eq!(normalize_title(s), normalize_artist(s), "mismatch for {s}");
        }
    }

    // ---- LibraryPairs::from_db ------------------------------------------

    #[test]
    fn from_db_returns_inverted_index_keyed_by_title() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.db");
        make_library_db(
            &db,
            &[
                ("Autechre", "Confield"),
                ("Autechre", "Amber"),
                ("Stereolab", "Aluminum Tunes"),
            ],
        );

        let pairs = LibraryPairs::from_db(&db).unwrap();
        assert_eq!(pairs.len(), 3);
        assert_eq!(
            pairs.artists_for_title("confield"),
            Some(&HashSet::from(["autechre".to_string()]))
        );
        assert_eq!(
            pairs.artists_for_title("aluminum tunes"),
            Some(&HashSet::from(["stereolab".to_string()]))
        );
    }

    #[test]
    fn from_db_collapses_duplicate_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.db");
        make_library_db(
            &db,
            &[
                ("Stereolab", "Aluminum Tunes"),
                ("Stereolab", "Aluminum Tunes"),
                ("Stereolab", "Aluminum Tunes"),
            ],
        );

        let pairs = LibraryPairs::from_db(&db).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs.pair_count(), 1);
    }

    #[test]
    fn from_db_groups_multiple_artists_under_same_title() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.db");
        make_library_db(
            &db,
            &[
                ("Various Artists", "Compilation"),
                ("Stereolab", "Compilation"),
            ],
        );

        let pairs = LibraryPairs::from_db(&db).unwrap();
        let set = pairs.artists_for_title("compilation").unwrap();
        assert_eq!(
            set,
            &HashSet::from(["various artists".to_string(), "stereolab".to_string()])
        );
    }

    #[test]
    fn from_db_normalizes_diacritics_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.db");
        make_library_db(&db, &[("Nilüfer Yanya", "PAINLESS")]);

        let pairs = LibraryPairs::from_db(&db).unwrap();
        assert_eq!(
            pairs.artists_for_title("painless"),
            Some(&HashSet::from(["nilufer yanya".to_string()]))
        );
    }

    #[test]
    fn from_db_skips_empty_artist_or_title_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.db");
        make_library_db(
            &db,
            &[
                ("", "Confield"),      // empty artist
                ("Autechre", ""),      // empty title
                ("   ", "  "),         // whitespace-only normalize to empty
                ("Autechre", "Amber"), // valid
            ],
        );

        let pairs = LibraryPairs::from_db(&db).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs.artists_for_title("amber"),
            Some(&HashSet::from(["autechre".to_string()]))
        );
    }

    #[test]
    fn from_db_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("nonexistent.db");
        let err = LibraryPairs::from_db(&db).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("library.db"), "unexpected error: {msg}");
    }

    // ---- LibraryPairs::matches ------------------------------------------

    fn pairs_from(rows: &[(&str, &str)]) -> LibraryPairs {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("library.db");
        make_library_db(&db, rows);
        LibraryPairs::from_db(&db).unwrap()
    }

    #[test]
    fn matches_exact_pair() {
        let pairs = pairs_from(&[("Autechre", "Confield")]);
        assert!(pairs.matches("Confield", ["Autechre"]));
    }

    #[test]
    fn matches_normalizes_diacritics_on_release_side() {
        // Library has the diacritic-stripped form; the Discogs release
        // arrives with diacritics. Both sides must match.
        let pairs = pairs_from(&[("Nilufer Yanya", "PAINLESS")]);
        assert!(pairs.matches("PAINLESS", ["Nilüfer Yanya"]));
    }

    #[test]
    fn matches_normalizes_diacritics_on_library_side() {
        // Library entry has diacritics, Discogs side doesn't.
        let pairs = pairs_from(&[("Nilüfer Yanya", "PAINLESS")]);
        assert!(pairs.matches("PAINLESS", ["Nilufer Yanya"]));
    }

    #[test]
    fn matches_rejects_when_title_not_in_library() {
        let pairs = pairs_from(&[("Autechre", "Confield")]);
        assert!(!pairs.matches("Some Other Album", ["Autechre"]));
    }

    #[test]
    fn matches_rejects_when_title_matches_but_artist_doesnt() {
        let pairs = pairs_from(&[("Autechre", "Confield")]);
        assert!(!pairs.matches("Confield", ["Some Other Band"]));
    }

    #[test]
    fn matches_kept_when_any_credited_artist_matches() {
        let pairs = pairs_from(&[("Field, The", "From Here We Go Sublime")]);
        assert!(pairs.matches("From Here We Go Sublime", ["Some Producer", "Field, The"]));
    }

    #[test]
    fn matches_empty_title_excluded() {
        let pairs = pairs_from(&[("Autechre", "Confield")]);
        assert!(!pairs.matches("", ["Autechre"]));
    }
}
