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
}

#[derive(Debug, Clone, Default)]
pub struct Format {
    pub name: String,
    pub qty: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseArtist {
    pub artist_id: u64,
    pub name: String,
    pub anv: String,
    pub join_field: String,
    pub position: u32,
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
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseImage {
    pub image_type: String,
    pub width: u32,
    pub height: u32,
    pub uri: String,
}
