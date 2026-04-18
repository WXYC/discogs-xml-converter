/// Data model for Discogs master release data.
///
/// These structs mirror the structure of a `<master>` element in the Discogs
/// masters XML data dump. Masters represent canonical album groupings that
/// link multiple release pressings/editions.

#[derive(Debug, Clone, Default)]
pub struct Master {
    pub id: u64,
    pub title: String,
    pub main_release_id: Option<u64>,
    pub year: Option<u16>,
    pub artists: Vec<MasterArtist>,
}

#[derive(Debug, Clone, Default)]
pub struct MasterArtist {
    pub id: u64,
    pub name: String,
}
