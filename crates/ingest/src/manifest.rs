use crate::store::Store;
use anyhow::Result;
use indexmap::IndexMap;
use pollywiki_schema::SourceStatus;
use serde::{Deserialize, Serialize};

const KEY: &str = "state/sync-manifest.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SyncManifest {
    #[serde(default)]
    pub sources: IndexMap<String, SourceStatus>,
}

pub async fn read_manifest(store: &Store) -> Result<SyncManifest> {
    Ok(store.get_json(KEY).await?.unwrap_or_default())
}

pub async fn record_sync(store: &Store, source: &str, ok: bool, note: Option<&str>) -> Result<()> {
    let mut manifest = read_manifest(store).await?;
    manifest.sources.insert(
        source.to_string(),
        SourceStatus {
            last_sync: crate::now_iso(),
            ok,
            note: note.map(str::to_string),
        },
    );
    store.put_json(KEY, &manifest).await
}
