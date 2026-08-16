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
        if !retryable || attempt >= 4 {
            return Err(anyhow!(
                "GET {url} failed: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ));
        }
        // 429s are per-minute quotas: short backoffs just burn more attempts.
        let backoff = if status.as_u16() == 429 {
            Duration::from_millis(attempt as u64 * 20_000)
        } else {
            Duration::from_millis(attempt as u64 * 2_000)
        };
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
    use std::io::{Read, Write};

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
}
