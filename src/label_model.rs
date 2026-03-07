/// Data model for Discogs label data.
///
/// These structs mirror the structure of a `<label>` element in the Discogs
/// labels XML data dump. Used to extract parent-child label relationships
/// for sublabel resolution during dedup.

#[derive(Debug, Clone, Default)]
pub struct Label {
    pub id: u64,
    pub name: String,
    pub parent_id: Option<u64>,
    pub parent_name: String,
}
