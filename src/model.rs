/// Data model for Discogs release data.
///
/// These structs mirror the structure of a `<release>` element in the Discogs
/// XML data dumps and are used as the intermediate representation between
/// XML parsing and CSV writing.

#[derive(Debug, Clone, Default)]
pub struct Release {
    pub id: u64,
    pub status: String,
    pub title: String,
    pub country: String,
    pub released: String,
    pub notes: String,
    pub data_quality: String,
    pub master_id: Option<u64>,
    pub formats: Vec<Format>,
    pub artists: Vec<ReleaseArtist>,
    pub extra_artists: Vec<ReleaseArtist>,
    pub labels: Vec<ReleaseLabel>,
    pub tracks: Vec<ReleaseTrack>,
    pub images: Vec<ReleaseImage>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    pub companies: Vec<ReleaseCompany>,
    pub videos: Vec<ReleaseVideo>,
}

impl Release {
    /// Build the format string from the list of formats.
    ///
    /// Rules (matching discogs-xml2db behavior):
    /// - Single format: just the name (e.g., "CD")
    /// - Format with qty > 1: "{qty}x{name}" (e.g., "2xLP")
    /// - Multiple formats: comma-separated (e.g., "CD, 2xLP")
    pub fn format_string(&self) -> String {
        self.formats
            .iter()
            .map(|f| {
                if f.qty > 1 {
                    format!("{}x{}", f.qty, f.name)
                } else {
                    f.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Build the flattened format-descriptions string.
    ///
    /// Discogs records the distinctions that separate one pressing from
    /// another — `Reissue`, `Repress`, `Remastered`, `Limited Edition`,
    /// `Album`/`Single`/`EP`/`Compilation`, and the vinyl sizes `7"` / `10"`
    /// / `12"` — as `<description>` children of `<format>`, not in the
    /// format `name`. A 7" single is `<format name="Vinyl">` with `7"` as a
    /// description, so without this the 7" and the 12" LP are the same row.
    /// See WXYC/discogs-xml-converter#91.
    ///
    /// Descriptions from every `<format>` are concatenated in document order
    /// with the same `", "` separator `format_string()` uses. Duplicates are
    /// kept: the column answers "does this contain Reissue" / "is this a 7\"",
    /// and collapsing would misrepresent a multi-format release.
    ///
    /// **The result is containment-queryable, never splittable.** The
    /// separator also occurs inside description values, so one description
    /// `33 ⅓ RPM, Stereo` is indistinguishable from the two descriptions
    /// `33 ⅓ RPM` and `Stereo`; and per-format attribution is lost, so a 7"
    /// + CD box set yields `7", Album` with no way to say which format the
    /// `7"` belongs to. That is the accepted cost of one flat column over a
    /// `release_format_description` child table — consumers test for
    /// containment, and anything needing per-description rows wants the
    /// child table instead of a parse of this string.
    ///
    /// Empty descriptions are dropped upstream in the parser, so the join
    /// never emits a doubled separator.
    pub fn format_descriptions_string(&self) -> String {
        self.formats
            .iter()
            .flat_map(|f| f.descriptions.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Format {
    pub name: String,
    pub qty: u32,
    /// `<format><descriptions><description>` values, in document order.
    /// Empty for a self-closing `<format />`, which has no children.
    pub descriptions: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseArtist {
    pub artist_id: u64,
    pub name: String,
    pub anv: String,
    pub join_field: String,
    pub position: u32,
    /// Discogs `<role>` element value for release-level `<extraartists>`
    /// children (e.g. "Producer", "Mixed By", "Written-By"). Always empty
    /// for `<artists>` (main-artist) entries. Mirrors `TrackArtist.role`;
    /// added so release-level credits reach `release_artist.role` for the
    /// release-level composer fallback (WXYC/library-metadata-lookup#699).
    pub role: String,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseLabel {
    pub name: String,
    pub catno: String,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseTrack {
    pub position: String,
    pub title: String,
    pub duration: String,
    pub artists: Vec<TrackArtist>,
    pub extra_artists: Vec<TrackArtist>,
}

#[derive(Debug, Clone, Default)]
pub struct TrackArtist {
    pub name: String,
    /// Discogs `<role>` element value for `<extraartists>` children
    /// (e.g. "Producer", "Mixed By", "Written-By"). Always empty for
    /// `<artists>` (main-artist) entries. See WXYC/discogs-etl#218.
    pub role: String,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseImage {
    pub image_type: String,
    pub width: u32,
    pub height: u32,
    pub uri: String,
}

impl wxyc_etl::pg::ImageRef for ReleaseImage {
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn uri(&self) -> &str {
        &self.uri
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseCompany {
    pub company_id: u64,
    pub name: String,
    pub entity_type: u32,
    pub entity_type_name: String,
}

#[derive(Debug, Clone)]
pub struct ReleaseVideo {
    pub src: String,
    pub title: String,
    pub duration: Option<u32>,
    pub embed: bool,
}

impl Default for ReleaseVideo {
    fn default() -> Self {
        ReleaseVideo {
            src: String::new(),
            title: String::new(),
            duration: None,
            embed: true,
        }
    }
}
