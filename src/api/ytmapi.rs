//! Placeholder for a future ytmapi-rs adapter.
//! The primary implementation uses our Innertube client (`innertube.rs`).

use crate::api::model::*;
use anyhow::Result;

pub async fn try_search_songs(_query: &str) -> Result<Vec<Track>> {
    anyhow::bail!("use innertube")
}
