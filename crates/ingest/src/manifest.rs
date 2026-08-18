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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/manifest-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[tokio::test]
    async fn an_absent_manifest_reads_as_empty() {
        let store = Store::Local(LocalStore::new(scratch("absent")));
        assert!(read_manifest(&store).await.unwrap().sources.is_empty());
    }

    #[tokio::test]
    async fn recording_a_sync_accumulates_and_overwrites_per_source() {
        let store = Store::Local(LocalStore::new(scratch("record")));

        record_sync(&store, "wikidata", true, None).await.unwrap();
        record_sync(&store, "tvfy", false, Some("no api key"))
            .await
            .unwrap();
        let manifest = read_manifest(&store).await.unwrap();
        assert_eq!(manifest.sources.len(), 2);
        assert!(manifest.sources["wikidata"].ok);
        assert!(!manifest.sources["tvfy"].ok);
        assert_eq!(manifest.sources["tvfy"].note.as_deref(), Some("no api key"));
        assert!(!manifest.sources["wikidata"].last_sync.is_empty());

        // A later run for the same source replaces its entry, and clears the note.
        record_sync(&store, "tvfy", true, None).await.unwrap();
        let manifest = read_manifest(&store).await.unwrap();
        assert_eq!(manifest.sources.len(), 2, "sources are keyed, not appended");
        assert!(manifest.sources["tvfy"].ok);
        assert!(manifest.sources["tvfy"].note.is_none());
    }
}
