//! Polite fetch: identifying UA, per-host rate limit, retry with backoff on 429/5xx.

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const USER_AGENT: &str = "pollywiki/0.1 (https://pollywiki.au; contact: logan@han.life)";

static LAST_REQUEST_AT: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client")
    })
}

#[derive(Clone, Default)]
pub struct FetchOpts {
    /// Minimum spacing between requests to the same host; defaults to 1000ms.
    pub min_interval_ms: Option<u64>,
    pub accept: Option<String>,
    pub headers: Vec<(String, String)>,
    pub post_json: Option<String>,
    /// Tries before a retryable status is given up on; defaults to 4.
    pub max_attempts: Option<u32>,
    /// Backoff base, multiplied by the attempt number. Defaults to 20s for a
    /// 429 (a per-minute quota needs real time to clear) and 2s otherwise.
    pub backoff_ms: Option<u64>,
}

impl FetchOpts {
    pub fn min_interval(ms: u64) -> Self {
        FetchOpts {
            min_interval_ms: Some(ms),
            ..Default::default()
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

pub async fn polite_fetch(url: &str, opts: &FetchOpts) -> Result<reqwest::Response> {
    let min_interval = Duration::from_millis(opts.min_interval_ms.unwrap_or(1000));
    let host = reqwest::Url::parse(url)?
        .host_str()
        .ok_or_else(|| anyhow!("no host in {url}"))?
        .to_string();

    let max_attempts = opts.max_attempts.unwrap_or(4);
    let mut attempt = 1u32;
    loop {
        let delay = {
            let mut guard = LAST_REQUEST_AT.lock().unwrap();
            let map = guard.get_or_insert_with(HashMap::new);
            let now = Instant::now();
            let wait = map
                .get(&host)
                .and_then(|last| (*last + min_interval).checked_duration_since(now))
                .unwrap_or(Duration::ZERO);
            map.insert(host.clone(), now + wait);
            wait
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        let req = match &opts.post_json {
            Some(body) => client()
                .post(url)
                .header("content-type", "application/json")
                .body(body.clone()),
            None => client().get(url),
        };
        // Later values replace earlier ones, so a caller-supplied user-agent
        // (browser strings for WAF-fronted sources) wins over the default.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("user-agent", USER_AGENT.parse()?);
        if let Some(accept) = &opts.accept {
            headers.insert("accept", accept.parse()?);
        }
        for (name, value) in &opts.headers {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
                value.parse()?,
            );
        }
        let req = req.headers(headers);

        let res = req.send().await?;
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let retryable = status.as_u16() == 429 || status.as_u16() >= 500;
        if !retryable || attempt >= max_attempts {
            return Err(anyhow!(
                "GET {url} failed: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ));
        }
        // 429s are per-minute quotas: short backoffs just burn more attempts.
        let base = opts.backoff_ms.unwrap_or(match status.as_u16() {
            429 => 20_000,
            _ => 2_000,
        });
        let backoff = Duration::from_millis(attempt as u64 * base);
        tokio::time::sleep(backoff).await;
        attempt += 1;
    }
}

pub async fn fetch_json<T: DeserializeOwned>(url: &str, opts: &FetchOpts) -> Result<T> {
    let mut with_accept = opts.clone();
    if with_accept.accept.is_none() {
        with_accept.accept = Some("application/json".to_string());
    }
    let res = polite_fetch(url, &with_accept).await?;
    Ok(res.json::<T>().await?)
}

pub async fn fetch_text(url: &str, opts: &FetchOpts) -> Result<String> {
    let res = polite_fetch(url, opts).await?;
    Ok(res.text().await?)
}

pub async fn fetch_bytes(url: &str, opts: &FetchOpts) -> Result<Vec<u8>> {
    let res = polite_fetch(url, opts).await?;
    Ok(res.bytes().await?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::{Response, TestServer};
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Retryable statuses with the waiting shrunk to nothing; production keeps
    /// the real backoff, which is measured in tens of seconds.
    fn fast() -> FetchOpts {
        FetchOpts {
            min_interval_ms: Some(1),
            backoff_ms: Some(1),
            ..Default::default()
        }
    }

    /// A caller-supplied user-agent must replace the default, never join it:
    /// WAF-fronted sources reject requests carrying two user-agent headers.
    #[tokio::test]
    async fn custom_user_agent_replaces_the_default() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                .unwrap();
            request
        });

        let opts = FetchOpts::min_interval(1).with_header("user-agent", "test-browser/1.0");
        let res = polite_fetch(&format!("http://{addr}/x"), &opts)
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 200);

        let request = server.join().unwrap();
        let ua_lines: Vec<&str> = request
            .lines()
            .filter(|l| l.to_lowercase().starts_with("user-agent:"))
            .collect();
        assert_eq!(
            ua_lines.len(),
            1,
            "exactly one user-agent header: {request}"
        );
        assert!(ua_lines[0].contains("test-browser/1.0"));
    }

    #[tokio::test]
    async fn a_retryable_status_is_retried_and_the_later_success_is_returned() {
        for status in [429u16, 500, 503] {
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&calls);
            let server = TestServer::start(move |_| match counter.fetch_add(1, Ordering::SeqCst) {
                0 | 1 => Response::status(status, "not yet"),
                _ => Response::text("done"),
            });
            let body = fetch_text(&format!("{}/x", server.base), &fast())
                .await
                .unwrap_or_else(|e| panic!("{status} should recover: {e}"));
            assert_eq!(body, "done");
            assert_eq!(calls.load(Ordering::SeqCst), 3, "{status} took two retries");
        }
    }

    #[tokio::test]
    async fn attempts_run_out_and_the_status_is_reported() {
        let server = TestServer::start(|_| Response::status(503, "still down"));
        let mut opts = fast();
        opts.max_attempts = Some(2);
        let err = fetch_text(&format!("{}/x", server.base), &opts)
            .await
            .expect_err("exhausted");
        assert!(err.to_string().contains("503"), "got {err}");
        assert_eq!(server.hits(), 2, "the attempt cap is honoured");
    }

    #[tokio::test]
    async fn a_client_error_fails_on_the_first_try() {
        for status in [400u16, 403, 404] {
            let server = TestServer::start(move |_| Response::status(status, "no"));
            let err = fetch_text(&format!("{}/x", server.base), &fast())
                .await
                .expect_err("client errors are final");
            assert!(err.to_string().contains(&status.to_string()), "got {err}");
            assert_eq!(server.hits(), 1, "{status} is not worth retrying");
        }
    }

    #[tokio::test]
    async fn json_asks_for_json_and_bytes_come_back_whole() {
        let server = TestServer::start(|req| match req.path.starts_with("/json") {
            true => Response::json(serde_json::json!({ "n": 7 }).to_string()),
            false => Response::bytes(vec![0u8, 159, 146, 150], "application/octet-stream"),
        });

        let value: serde_json::Value = fetch_json(&format!("{}/json", server.base), &fast())
            .await
            .expect("json");
        assert_eq!(value["n"], 7);

        // Not valid utf-8, so it must survive as bytes rather than as text.
        let bytes = fetch_bytes(&format!("{}/bin", server.base), &fast())
            .await
            .expect("bytes");
        assert_eq!(bytes, vec![0u8, 159, 146, 150]);

        let sent = server.requests();
        assert_eq!(
            sent[0].header("accept"),
            Some("application/json"),
            "fetch_json defaults the accept header"
        );
        assert_eq!(sent[0].method, "GET");
    }

    #[tokio::test]
    async fn a_caller_supplied_accept_header_is_not_overwritten() {
        let server = TestServer::start(|_| Response::json("{}"));
        let mut opts = fast();
        opts.accept = Some("application/sparql-results+json".to_string());
        let _: serde_json::Value = fetch_json(&format!("{}/q", server.base), &opts)
            .await
            .expect("json");
        assert_eq!(
            server.requests()[0].header("accept"),
            Some("application/sparql-results+json")
        );
    }

    #[tokio::test]
    async fn requests_to_one_host_are_spaced_by_the_minimum_interval() {
        let server = TestServer::start(|_| Response::text("ok"));
        let opts = FetchOpts::min_interval(120);
        let started = Instant::now();
        for _ in 0..3 {
            fetch_text(&format!("{}/x", server.base), &opts)
                .await
                .expect("ok");
        }
        // Three requests means two gaps; the first goes out immediately.
        assert!(
            started.elapsed() >= Duration::from_millis(240),
            "spacing collapsed: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_url_with_no_host_is_rejected_before_any_request() {
        let err = fetch_text("file:///etc/hosts", &fast())
            .await
            .expect_err("no host");
        assert!(err.to_string().contains("no host"), "got {err}");
    }
}
