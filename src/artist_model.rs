/// Data model for Discogs artist data.
///
/// These structs mirror the structure of an `<artist>` element in the Discogs
/// artists XML data dump. Used to extract aliases, name variations, group
/// membership, and external URLs for enhanced artist filtering and metadata
/// enrichment.

#[derive(Debug, Clone, Default)]
pub struct Artist {
    pub id: u64,
    pub name: String,
    pub profile: String,
    pub aliases: Vec<String>,
    pub name_variations: Vec<String>,
    pub members: Vec<Member>,
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Member {
    pub id: u64,
    pub name: String,
}
