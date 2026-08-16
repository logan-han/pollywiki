//! Single seam for all persistence. The S3 implementation is canonical;
//! the local one mirrors the same key layout under .store/ for development.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub enum Store {
    Local(LocalStore),
    S3(S3Store),
}

impl Store {
    pub async fn put_raw(&self, key: &str, body: &[u8]) -> Result<()> {
        match self {
            Store::Local(s) => s.put_raw(key, body).await,
            Store::S3(s) => s.put_raw(key, body).await,
        }
    }

    pub async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        match self {
            Store::Local(s) => s.get_raw(key).await,
            Store::S3(s) => s.get_raw(key).await,
        }
    }

    pub async fn put_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let body = match self {
            // Mirrors the development store's human-readable one-space indent.
            Store::Local(_) => pretty_json(value)?,
            Store::S3(_) => serde_json::to_string(value)?,
        };
        self.put_raw(key, body.as_bytes()).await
    }

    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get_raw(key).await? {
            None => Ok(None),
            Some(raw) => Ok(Some(
                serde_json::from_str(&raw).with_context(|| format!("parsing {key}"))?,
            )),
        }
    }

    pub async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        match self {
            Store::Local(s) => s.list(prefix).await,
            Store::S3(s) => s.list(prefix).await,
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        match self {
            Store::Local(s) => s.delete(key).await,
            Store::S3(s) => s.delete(key).await,
        }
    }
}

fn pretty_json<T: Serialize>(value: &T) -> Result<String> {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    value.serialize(&mut ser)?;
    Ok(String::from_utf8(out)?)
}

pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalStore { root: root.into() }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    async fn put_raw(&self, key: &str, body: &[u8]) -> Result<()> {
        let file = self.path(key);
        if let Some(dir) = file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(&file, body).await?;
        Ok(())
    }

    async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        match tokio::fs::read_to_string(self.path(key)).await {
            Ok(raw) => Ok(Some(raw)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        match tokio::fs::remove_file(self.path(key)).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let dir = self.path(prefix);
        let mut keys = Vec::new();
        walk(&dir, prefix, &dir.clone(), &mut keys)?;
        keys.sort();
        Ok(keys)
    }
}

fn walk(dir: &Path, prefix: &str, base: &Path, keys: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let full = entry.path();
        if entry.file_type()?.is_dir() {
            walk(&full, prefix, base, keys)?;
        } else {
            let rel = full
                .strip_prefix(base)?
                .to_string_lossy()
                .replace('\\', "/");
            let joined = if prefix.is_empty() {
                rel
            } else if prefix.ends_with('/') {
                format!("{prefix}{rel}")
            } else {
                format!("{prefix}/{rel}")
            };
            keys.push(joined);
        }
    }
    Ok(())
}

/// Accepts "bucket" or "bucket/prefix" (e.g. "pollywiki.au/data") so the data
/// store can share a bucket with the published site under separate prefixes.
pub struct S3Store {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

impl S3Store {
    pub async fn new(bucket_with_prefix: &str) -> Result<Self> {
        let mut parts = bucket_with_prefix.splitn(2, '/');
        let bucket = parts.next().unwrap_or_default().to_string();
        if bucket.is_empty() {
            anyhow::bail!("invalid bucket: {bucket_with_prefix}");
        }
        let prefix = match parts.next() {
            Some(rest) if !rest.is_empty() => format!("{rest}/"),
            _ => String::new(),
        };
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "ap-southeast-2".to_string());
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region))
            .load()
            .await;
        Ok(S3Store {
            client: aws_sdk_s3::Client::new(&config),
            bucket,
            prefix,
        })
    }

    async fn put_raw(&self, key: &str, body: &[u8]) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(format!("{}{key}", self.prefix))
            .body(body.to_vec().into())
            .content_type(content_type_for(key))
            .send()
            .await?;
        Ok(())
    }

    async fn get_raw(&self, key: &str) -> Result<Option<String>> {
        let res = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(format!("{}{key}", self.prefix))
            .send()
            .await;
        match res {
            Ok(out) => {
                let bytes = out.body.collect().await?.into_bytes();
                Ok(Some(String::from_utf8(bytes.to_vec())?))
            }
            Err(err) => {
                let service = err.into_service_error();
                if service.is_no_such_key() {
                    Ok(None)
                } else {
                    Err(service.into())
                }
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(format!("{}{key}", self.prefix))
            .send()
            .await?;
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(format!("{}{prefix}", self.prefix));
            if let Some(t) = &token {
                req = req.continuation_token(t);
            }
            let res = req.send().await?;
            for obj in res.contents() {
                if let Some(key) = obj.key() {
                    keys.push(key[self.prefix.len()..].to_string());
                }
            }
            token = res.next_continuation_token().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        keys.sort();
        Ok(keys)
    }
}

fn content_type_for(key: &str) -> &'static str {
    if key.ends_with(".json") {
        "application/json"
    } else if key.ends_with(".jsonl") {
        "application/x-ndjson"
    } else if key.ends_with(".csv") {
        "text/csv"
    } else if key.ends_with(".xml") {
        "application/xml"
    } else if key.ends_with(".jpg") || key.ends_with(".jpeg") {
        "image/jpeg"
    } else if key.ends_with(".png") {
        "image/png"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_match_key_suffixes() {
        assert_eq!(
            content_type_for("bundles/people.jsonl"),
            "application/x-ndjson"
        );
        assert_eq!(
            content_type_for("state/sync-manifest.json"),
            "application/json"
        );
        assert_eq!(content_type_for("raw/aec/x.csv"), "text/csv");
        assert_eq!(
            content_type_for("derived/img/people/a-96.jpg"),
            "image/jpeg"
        );
        assert_eq!(content_type_for("mystery.bin"), "application/octet-stream");
    }

    #[tokio::test]
    async fn local_store_round_trips_and_lists() {
        let dir = std::env::temp_dir().join(format!("pollywiki-store-test-{}", std::process::id()));
        let store = Store::Local(LocalStore::new(&dir));
        store
            .put_json("canonical/a/x.json", &serde_json::json!({"n": 1}))
            .await
            .unwrap();
        store
            .put_json("canonical/a/b/y.json", &serde_json::json!({"n": 2}))
            .await
            .unwrap();
        assert_eq!(
            store.list("canonical/a/").await.unwrap(),
            vec![
                "canonical/a/b/y.json".to_string(),
                "canonical/a/x.json".to_string()
            ]
        );
        let value: serde_json::Value = store.get_json("canonical/a/x.json").await.unwrap().unwrap();
        assert_eq!(value["n"], 1);
        assert_eq!(store.get_raw("canonical/missing.json").await.unwrap(), None);
        store.delete("canonical/a/x.json").await.unwrap();
        assert_eq!(store.get_raw("canonical/a/x.json").await.unwrap(), None);
        store.delete("canonical/a/x.json").await.unwrap(); // deleting again is fine
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
