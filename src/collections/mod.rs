//! Collection graph adoption; effects and HTTP collection workflows are separate consumers.
pub mod repository;

#[derive(Clone, Copy, Default, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CollectionMonitoring {
    #[default]
    MovieOnly,
    MovieAndCollection,
    None,
}
