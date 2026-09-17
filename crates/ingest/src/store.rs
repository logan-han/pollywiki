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

/// Splits "bucket" or "bucket/prefix" into the bucket and a key prefix that
/// already ends in a slash.
fn split_bucket(bucket_with_prefix: &str) -> Result<(String, String)> {
    let mut parts = bucket_with_prefix.splitn(2, '/');
    let bucket = parts.next().unwrap_or_default().to_string();
    if bucket.is_empty() {
        anyhow::bail!("invalid bucket: {bucket_with_prefix}");
    }
    let prefix = match parts.next() {
        Some(rest) if !rest.is_empty() => format!("{rest}/"),
        _ => String::new(),
    };
    Ok((bucket, prefix))
}

impl S3Store {
    pub async fn new(bucket_with_prefix: &str) -> Result<Self> {
        let (bucket, prefix) = split_bucket(bucket_with_prefix)?;
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

    /// The same store against a client the caller configured. Tests point one
    /// at a local stub so every request the real one makes is exercised.
    #[cfg(test)]
    fn with_client(client: aws_sdk_s3::Client, bucket_with_prefix: &str) -> Result<Self> {
        let (bucket, prefix) = split_bucket(bucket_with_prefix)?;
        Ok(S3Store {
            client,
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

#[cfg(test)]
mod local_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/store-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[tokio::test]
    async fn json_is_written_one_space_indented_and_reads_back() {
        let store = Store::Local(LocalStore::new(scratch("json")));
        let value = serde_json::json!({ "b": 1, "a": [1, 2] });
        store
            .put_json("canonical/thing.json", &value)
            .await
            .unwrap();

        // The on-disk shape is the pretty form the bundles are diffed in.
        let raw = store
            .get_raw("canonical/thing.json")
            .await
            .unwrap()
            .unwrap();
        assert!(raw.starts_with("{\n \"b\": 1"), "unexpected shape: {raw}");

        let back: serde_json::Value = store
            .get_json("canonical/thing.json")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(back, value);
    }

    #[tokio::test]
    async fn missing_keys_read_as_none_and_deletes_are_idempotent() {
        let store = Store::Local(LocalStore::new(scratch("missing")));
        let missing: Option<serde_json::Value> = store.get_json("nope.json").await.unwrap();
        assert!(missing.is_none());
        assert!(store.get_raw("nope.json").await.unwrap().is_none());
        // Deleting something that was never there is not an error.
        store.delete("nope.json").await.unwrap();
    }

    #[tokio::test]
    async fn listing_walks_nested_prefixes_and_sorts() {
        let store = Store::Local(LocalStore::new(scratch("list")));
        for key in [
            "canonical/people/b.json",
            "canonical/people/a.json",
            "canonical/people/nested/c.json",
            "canonical/other/d.json",
        ] {
            store.put_raw(key, b"{}").await.unwrap();
        }
        let keys = store.list("canonical/people/").await.unwrap();
        assert_eq!(
            keys,
            vec![
                "canonical/people/a.json",
                "canonical/people/b.json",
                "canonical/people/nested/c.json",
            ],
            "listing must stay inside the prefix and be sorted"
        );
        // An empty prefix lists nothing rather than failing.
        assert!(store.list("canonical/absent/").await.unwrap().is_empty());
    }
}

/// The S3 half of the store, driven against a local stub that speaks just
/// enough of the API to answer it. Everything the real client sends -- path
/// style, signed, with checksums -- is sent here too; only the far end is
/// local, so the request-building and response-parsing this file does are
/// covered without an AWS account.
#[cfg(test)]
mod s3_tests {
    use super::*;
    use crate::test_http::{Request, Response, TestServer};
    use indexmap::IndexMap;
    use std::sync::{Arc, Mutex};

    type Objects = Arc<Mutex<IndexMap<String, String>>>;

    fn xml(status: u16, body: String) -> Response {
        Response {
            status,
            content_type: "application/xml".to_string(),
            body: body.into_bytes(),
        }
    }

    fn no_such_key(key: &str) -> Response {
        xml(
            404,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchKey</Code>\
                 <Message>The specified key does not exist.</Message><Key>{key}</Key>\
                 <RequestId>stub</RequestId><HostId>stub</HostId></Error>"
            ),
        )
    }

    /// Path-style key: "/bucket/some/key" with the bucket dropped.
    fn key_of(request: &Request) -> String {
        let path = request.path.split('?').next().unwrap_or_default();
        path.trim_start_matches('/')
            .split_once('/')
            .map(|(_bucket, key)| key.to_string())
            .unwrap_or_default()
    }

    fn list_response(objects: &IndexMap<String, String>, request: &Request) -> Response {
        let prefix = request
            .query("prefix")
            .map(|p| p.replace("%2F", "/"))
            .unwrap_or_default();
        let mut matching: Vec<&String> =
            objects.keys().filter(|k| k.starts_with(&prefix)).collect();
        matching.sort();
        // One key per page, so the continuation loop runs for real. The token
        // is an index rather than a key: S3 treats it as opaque, and a bare
        // number survives the round trip through the query string intact.
        let start: usize = request
            .query("continuation-token")
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        let page = matching.get(start).copied();
        let truncated = start + 1 < matching.len();

        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>bucket</Name><MaxKeys>1</MaxKeys>",
        );
        if let Some(key) = page {
            body.push_str(&format!(
                "<Contents><Key>{key}</Key><Size>{}</Size>\
                 <LastModified>2026-01-01T00:00:00.000Z</LastModified>\
                 <StorageClass>STANDARD</StorageClass></Contents>",
                objects[key].len()
            ));
        }
        body.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
        if truncated {
            body.push_str(&format!(
                "<NextContinuationToken>{}</NextContinuationToken>",
                start + 1
            ));
        }
        body.push_str("</ListBucketResult>");
        xml(200, body)
    }

    /// A bucket in a mutex. `denied/` is the one key that answers with
    /// something other than a missing object.
    fn stub_s3(objects: Objects) -> TestServer {
        TestServer::start(move |request| {
            let mut objects = objects.lock().expect("stub bucket");
            let key = key_of(request);
            match request.method.as_str() {
                _ if request.query("list-type").is_some() => list_response(&objects, request),
                "PUT" => {
                    objects.insert(key, request.body.clone());
                    Response {
                        status: 200,
                        content_type: "application/xml".to_string(),
                        body: Vec::new(),
                    }
                }
                "GET" if key.contains("denied") => xml(
                    403,
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>AccessDenied</Code>\
                     <Message>Access Denied</Message><RequestId>stub</RequestId>\
                     <HostId>stub</HostId></Error>"
                        .to_string(),
                ),
                "GET" => match objects.get(&key) {
                    Some(body) => Response {
                        status: 200,
                        content_type: "application/octet-stream".to_string(),
                        body: body.clone().into_bytes(),
                    },
                    None => no_such_key(&key),
                },
                "DELETE" => {
                    objects.shift_remove(&key);
                    xml(204, String::new())
                }
                other => xml(400, format!("<Error><Code>{other}</Code></Error>")),
            }
        })
    }

    fn s3_store(base: &str, bucket_with_prefix: &str) -> Store {
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("ap-southeast-2"))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test-key",
                "test-secret",
                None,
                None,
                "stub",
            ))
            .endpoint_url(base)
            .force_path_style(true)
            .build();
        Store::S3(
            S3Store::with_client(aws_sdk_s3::Client::from_conf(config), bucket_with_prefix)
                .expect("stub store"),
        )
    }

    #[test]
    fn a_bucket_may_carry_a_prefix_and_must_not_be_empty() {
        assert_eq!(
            split_bucket("pollywiki.au/data").unwrap(),
            ("pollywiki.au".to_string(), "data/".to_string())
        );
        // No prefix, and a trailing slash with nothing after it, are the same.
        assert_eq!(
            split_bucket("pollywiki.au").unwrap(),
            ("pollywiki.au".to_string(), String::new())
        );
        assert_eq!(
            split_bucket("pollywiki.au/").unwrap(),
            ("pollywiki.au".to_string(), String::new())
        );
        assert_eq!(
            split_bucket("/data").unwrap_err().to_string(),
            "invalid bucket: /data"
        );
    }

    #[tokio::test]
    async fn objects_round_trip_under_the_configured_prefix() {
        let objects: Objects = Arc::new(Mutex::new(IndexMap::new()));
        let server = stub_s3(Arc::clone(&objects));
        let store = s3_store(&server.base, "pollywiki.au/data");

        store
            .put_json("canonical/people/alex.json", &serde_json::json!({ "n": 1 }))
            .await
            .expect("put");

        // The prefix is part of the key on the wire, and never part of the key
        // the rest of the ingest deals in.
        let stored: Vec<(String, String)> = {
            let bucket = objects.lock().expect("stub bucket");
            bucket.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        assert_eq!(
            stored,
            // S3 gets the compact form; only the local store pretty-prints.
            vec![(
                "data/canonical/people/alex.json".to_string(),
                r#"{"n":1}"#.to_string()
            )]
        );

        let value: serde_json::Value = store
            .get_json("canonical/people/alex.json")
            .await
            .expect("get")
            .expect("present");
        assert_eq!(value["n"], 1);

        store
            .delete("canonical/people/alex.json")
            .await
            .expect("delete");
        assert!(objects.lock().expect("stub bucket").is_empty());
    }

    #[tokio::test]
    async fn a_missing_key_reads_as_none_and_any_other_error_surfaces() {
        let server = stub_s3(Arc::new(Mutex::new(IndexMap::new())));
        let store = s3_store(&server.base, "pollywiki.au");

        assert!(store
            .get_raw("canonical/absent.json")
            .await
            .expect("a missing object is not an error")
            .is_none());

        let err = store
            .get_raw("canonical/denied.json")
            .await
            .expect_err("anything else is");
        assert!(
            err.to_string().contains("AccessDenied") || err.to_string().contains("403"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn listing_follows_the_continuation_token_to_the_end() {
        let objects: Objects = Arc::new(Mutex::new(IndexMap::new()));
        let server = stub_s3(Arc::clone(&objects));
        let store = s3_store(&server.base, "pollywiki.au/data");

        for slug in ["c", "a", "b"] {
            store
                .put_raw(&format!("canonical/people/{slug}.json"), b"{}")
                .await
                .expect("put");
        }
        store
            .put_raw("canonical/bills/s1.json", b"{}")
            .await
            .expect("put");

        // The stub pages one key at a time, so three pages are walked.
        let keys = store.list("canonical/people/").await.expect("list");
        assert_eq!(
            keys,
            vec![
                "canonical/people/a.json",
                "canonical/people/b.json",
                "canonical/people/c.json",
            ],
            "the store prefix is stripped and the listing stays inside the key prefix"
        );
    }
}
