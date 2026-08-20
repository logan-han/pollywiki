//! A local HTTP server for tests, so every source can be driven end to end
//! without touching the network. Each source's endpoint is a value rather than
//! a constant precisely so these tests can point it here.
//!
//! One request per connection, closed straight after: keep-alive would let a
//! pooled reqwest connection outlive the handler and hang the accept loop.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path with the query string still attached, as it arrived.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }

    /// One query parameter, undecoded. Enough for the `?page=2` style checks
    /// these tests make.
    pub fn query(&self, name: &str) -> Option<String> {
        let query = self.path.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| value.to_string())
        })
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(body: impl Into<String>) -> Self {
        Response {
            status: 200,
            content_type: "application/json".to_string(),
            body: body.into().into_bytes(),
        }
    }

    pub fn text(body: impl Into<String>) -> Self {
        Response {
            status: 200,
            content_type: "text/plain; charset=utf-8".to_string(),
            body: body.into().into_bytes(),
        }
    }

    pub fn html(body: impl Into<String>) -> Self {
        Response {
            status: 200,
            content_type: "text/html; charset=utf-8".to_string(),
            body: body.into().into_bytes(),
        }
    }

    pub fn bytes(body: Vec<u8>, content_type: &str) -> Self {
        Response {
            status: 200,
            content_type: content_type.to_string(),
            body,
        }
    }

    pub fn status(status: u16, body: impl Into<String>) -> Self {
        Response {
            status,
            content_type: "text/plain; charset=utf-8".to_string(),
            body: body.into().into_bytes(),
        }
    }
}

pub struct TestServer {
    /// Origin with no trailing slash, e.g. "http://127.0.0.1:52344".
    pub base: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stopping: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    /// Serves `handler` until dropped. The handler sees every request in
    /// arrival order and decides the response, so a test can vary by path,
    /// by body, or by how many calls have already landed.
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&Request) -> Response + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));

        let worker_requests = Arc::clone(&requests);
        let worker_stopping = Arc::clone(&stopping);
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stopping.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let response = handler(&request);
                worker_requests.lock().unwrap().push(request);
                let head = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    response.status,
                    reason(response.status),
                    response.content_type,
                    response.body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&response.body);
                let _ = stream.flush();
            }
        });

        TestServer {
            base,
            requests,
            stopping,
            worker: Some(worker),
        }
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // The accept loop is blocked; one throwaway connection wakes it so it
        // can see the flag and return.
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// Reads one request: headers to the blank line, then exactly the body the
/// content-length announces.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut raw: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => raw.push(byte[0]),
            Err(_) => return None,
        }
    }
    let head = String::from_utf8_lossy(&raw).to_string();
    let mut lines = head.lines();
    let mut start = lines.next()?.split_whitespace();
    let method = start.next()?.to_string();
    let path = start.next()?.to_string();

    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 && stream.read_exact(&mut body).is_err() {
        return None;
    }

    Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_server_records_requests_and_answers_from_the_handler() {
        let server = TestServer::start(|req| match req.path.starts_with("/echo") {
            true => Response::json(serde_json::json!({ "body": req.body }).to_string()),
            false => Response::status(404, "no"),
        });
        let opts = crate::http::FetchOpts {
            min_interval_ms: Some(1),
            post_json: Some("{\"a\":1}".to_string()),
            ..Default::default()
        };
        let body = crate::http::fetch_text(&format!("{}/echo?x=1", server.base), &opts)
            .await
            .expect("echo");
        assert_eq!(body, r#"{"body":"{\"a\":1}"}"#);

        let miss = crate::http::fetch_text(&format!("{}/nope", server.base), &opts).await;
        assert!(miss.is_err(), "404 surfaces as an error");

        let seen = server.requests();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].query("x").as_deref(), Some("1"));
        assert_eq!(seen[0].header("content-type"), Some("application/json"));
    }
}
