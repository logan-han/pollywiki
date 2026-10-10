//! Langfuse tracing for every model call, the way hearth does it: one
//! OpenTelemetry span per call to Langfuse's v4 OTLP endpoint, switched on by
//! the Langfuse keys and otherwise inert. Spans go out as OTLP/HTTP JSON over
//! the existing HTTP client, so no OpenTelemetry SDK is pulled in for one kind
//! of span, and a failed export is a warning, never a failed run.

use crate::http::{polite_fetch, FetchOpts};
use base64::Engine;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_BASE_URL: &str = "https://cloud.langfuse.com";

#[derive(Debug, Clone)]
pub struct Langfuse {
    endpoint: String,
    authorization: String,
    environment: String,
    release: Option<String>,
    session: Option<String>,
    record_content: bool,
}

/// Token counts in the exclusive buckets Langfuse adds up: cached prompt
/// tokens apart from the rest of the prompt, thinking apart from the reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: i64,
    pub input_cached: i64,
    pub output: i64,
    pub output_reasoning: i64,
}

impl Usage {
    /// Gemini's usageMetadata, where the cached count is part of the prompt
    /// count and thoughts are counted beside the candidates.
    pub fn from_gemini(response: &Value) -> Option<Usage> {
        let meta = response.get("usageMetadata")?;
        let count = |key: &str| meta.get(key).and_then(Value::as_i64).unwrap_or(0);
        let cached = count("cachedContentTokenCount");
        Some(Usage {
            input: (count("promptTokenCount") - cached).max(0),
            input_cached: cached,
            output: count("candidatesTokenCount"),
            output_reasoning: count("thoughtsTokenCount"),
        })
    }

    fn details(&self) -> Value {
        let mut out = json!({ "input": self.input, "output": self.output });
        if self.input_cached > 0 {
            out["input_cached_tokens"] = json!(self.input_cached);
        }
        if self.output_reasoning > 0 {
            out["output_reasoning_tokens"] = json!(self.output_reasoning);
        }
        out
    }
}

/// One model call as it happened.
pub struct ModelCall<'a> {
    /// What the call was for, e.g. "pollywiki.bill-notes"; names the trace.
    pub name: &'a str,
    pub model: &'a str,
    pub parameters: &'a Value,
    pub prompt: &'a str,
    pub outcome: Result<&'a str, String>,
    pub usage: Option<Usage>,
    pub started: SystemTime,
    pub ended: SystemTime,
    pub metadata: &'a [(&'a str, String)],
}

impl Langfuse {
    pub fn from_env() -> Option<Langfuse> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// An empty value counts as unset, as a blank line copied from an
    /// example file or a workflow secret that was never added would be.
    fn from_vars(get: impl Fn(&str) -> Option<String>) -> Option<Langfuse> {
        let var = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let public = var("LANGFUSE_PUBLIC_KEY")?;
        let secret = var("LANGFUSE_SECRET_KEY")?;
        let base = var("LANGFUSE_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Some(Langfuse {
            endpoint: format!("{}/api/public/otel/v1/traces", base.trim_end_matches('/')),
            authorization: format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{public}:{secret}"))
            ),
            environment: var("LANGFUSE_TRACING_ENVIRONMENT")
                .unwrap_or_else(|| "development".to_string()),
            release: var("GITHUB_SHA").map(|sha| sha.chars().take(7).collect()),
            // One session per workflow run, so a night's calls read together.
            session: var("GITHUB_RUN_ID").map(|id| format!("ingest-{id}")),
            record_content: var("LANGFUSE_RECORD_CONTENT")
                .is_none_or(|v| !v.eq_ignore_ascii_case("off")),
        })
    }

    /// Tracing pointed at a local server, with content recorded.
    #[cfg(test)]
    pub fn at(base_url: &str) -> Langfuse {
        Self::from_vars(|name| match name {
            "LANGFUSE_PUBLIC_KEY" => Some("pk-test".to_string()),
            "LANGFUSE_SECRET_KEY" => Some("sk-test".to_string()),
            "LANGFUSE_BASE_URL" => Some(base_url.to_string()),
            _ => None,
        })
        .expect("both keys are set")
    }

    /// Sends one call to Langfuse. Never fails the caller.
    pub async fn record(&self, call: &ModelCall<'_>) {
        let mut opts = FetchOpts::min_interval(0)
            .with_header("authorization", &self.authorization)
            .with_header("x-langfuse-ingestion-version", "4");
        opts.post_json = Some(self.payload(call).to_string());
        opts.max_attempts = Some(2);
        if let Err(err) = polite_fetch(&self.endpoint, &opts).await {
            eprintln!("telemetry: Langfuse export failed: {err}");
        }
    }

    /// The OTLP/JSON request for one call: a generation span carrying its
    /// trace's attributes too, since Langfuse v4 queries observations alone.
    fn payload(&self, call: &ModelCall<'_>) -> Value {
        let mut attributes: Vec<Value> = Vec::new();
        let mut put = |key: &str, value: &str| {
            attributes.push(json!({ "key": key, "value": { "stringValue": value } }));
        };
        put("langfuse.trace.name", call.name);
        put("langfuse.observation.type", "generation");
        put("langfuse.environment", &self.environment);
        if let Some(release) = &self.release {
            put("langfuse.release", release);
        }
        if let Some(session) = &self.session {
            put("langfuse.session.id", session);
        }
        put("langfuse.observation.model.name", call.model);
        put(
            "langfuse.observation.model.parameters",
            &call.parameters.to_string(),
        );
        if self.record_content {
            put("langfuse.observation.input", call.prompt);
            if let Ok(output) = call.outcome {
                put("langfuse.observation.output", output);
            }
        }
        if let Some(usage) = call.usage {
            put(
                "langfuse.observation.usage_details",
                &usage.details().to_string(),
            );
        }
        for (key, value) in call.metadata {
            put(&format!("langfuse.trace.metadata.{key}"), value);
            put(&format!("langfuse.observation.metadata.{key}"), value);
        }
        let status = match &call.outcome {
            Ok(_) => json!({ "code": 1 }),
            Err(message) => {
                put("langfuse.observation.level", "ERROR");
                put("langfuse.observation.status_message", message);
                json!({ "code": 2, "message": message })
            }
        };
        json!({ "resourceSpans": [{
            "resource": { "attributes": [
                { "key": "service.name", "value": { "stringValue": "pollywiki" } }
            ] },
            "scopeSpans": [{
                "scope": { "name": "pollywiki-ingest" },
                "spans": [{
                    "traceId": random_hex(16),
                    "spanId": random_hex(8),
                    "name": call.name,
                    // SPAN_KIND_CLIENT: an outgoing call to the model's API.
                    "kind": 3,
                    "startTimeUnixNano": unix_nanos(call.started),
                    "endTimeUnixNano": unix_nanos(call.ended),
                    "attributes": attributes,
                    "status": status,
                }],
            }],
        }] })
    }
}

fn unix_nanos(at: SystemTime) -> String {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string()
}

/// OTLP's JSON encoding takes trace and span ids as lowercase hex.
fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", fastrand::u8(..)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn vars(pairs: &[(&str, &str)]) -> Option<Langfuse> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Langfuse::from_vars(|name| map.get(name).cloned())
    }

    fn attribute<'v>(span: &'v Value, key: &str) -> Option<&'v str> {
        span["attributes"]
            .as_array()?
            .iter()
            .find(|a| a["key"] == key)?["value"]["stringValue"]
            .as_str()
    }

    fn call<'a>(
        outcome: Result<&'a str, String>,
        metadata: &'a [(&'a str, String)],
    ) -> ModelCall<'a> {
        static PARAMETERS: std::sync::LazyLock<Value> =
            std::sync::LazyLock::new(|| json!({ "temperature": 0.2 }));
        let started = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        ModelCall {
            name: "pollywiki.bill-notes",
            model: "test-model",
            parameters: &PARAMETERS,
            prompt: "Explain these bills",
            outcome,
            usage: Some(Usage {
                input: 90,
                input_cached: 10,
                output: 40,
                output_reasoning: 0,
            }),
            started,
            ended: started + Duration::from_millis(1500),
            metadata,
        }
    }

    #[test]
    fn tracing_is_off_without_both_keys_and_a_blank_key_is_no_key() {
        assert!(vars(&[]).is_none());
        assert!(vars(&[("LANGFUSE_PUBLIC_KEY", "pk")]).is_none());
        assert!(vars(&[("LANGFUSE_PUBLIC_KEY", "pk"), ("LANGFUSE_SECRET_KEY", " ")]).is_none());

        let on = vars(&[("LANGFUSE_PUBLIC_KEY", "pk"), ("LANGFUSE_SECRET_KEY", "sk")])
            .expect("both keys switch it on");
        assert_eq!(
            on.endpoint,
            "https://cloud.langfuse.com/api/public/otel/v1/traces"
        );
        assert_eq!(on.authorization, "Basic cGs6c2s=", "base64 of pk:sk");
        assert_eq!(on.environment, "development");
        assert!(on.record_content, "content is recorded unless switched off");
        assert!(on.release.is_none() && on.session.is_none());
    }

    #[test]
    fn a_workflow_run_names_its_release_session_and_environment() {
        let on = vars(&[
            ("LANGFUSE_PUBLIC_KEY", "pk"),
            ("LANGFUSE_SECRET_KEY", "sk"),
            ("LANGFUSE_BASE_URL", "https://us.cloud.langfuse.com/"),
            ("LANGFUSE_TRACING_ENVIRONMENT", "production"),
            ("LANGFUSE_RECORD_CONTENT", "OFF"),
            ("GITHUB_SHA", "0123456789abcdef"),
            ("GITHUB_RUN_ID", "42"),
        ])
        .expect("on");
        assert_eq!(
            on.endpoint,
            "https://us.cloud.langfuse.com/api/public/otel/v1/traces"
        );
        assert_eq!(on.environment, "production");
        assert_eq!(on.release.as_deref(), Some("0123456"));
        assert_eq!(on.session.as_deref(), Some("ingest-42"));
        assert!(!on.record_content);
    }

    #[test]
    fn a_call_is_one_generation_span_with_its_trace_attributes() {
        let on = vars(&[
            ("LANGFUSE_PUBLIC_KEY", "pk"),
            ("LANGFUSE_SECRET_KEY", "sk"),
            ("GITHUB_RUN_ID", "7"),
        ])
        .expect("on");
        let metadata = [("bills", "r1,r2".to_string())];
        let payload = on.payload(&call(Ok("[]"), &metadata));
        let span = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];

        assert_eq!(span["name"], "pollywiki.bill-notes");
        assert_eq!(span["traceId"].as_str().map(str::len), Some(32));
        assert_eq!(span["spanId"].as_str().map(str::len), Some(16));
        assert_eq!(span["startTimeUnixNano"], "1800000000000000000");
        assert_eq!(span["endTimeUnixNano"], "1800000001500000000");
        assert_eq!(span["status"]["code"], 1);
        assert_eq!(
            attribute(span, "langfuse.trace.name"),
            Some("pollywiki.bill-notes")
        );
        assert_eq!(
            attribute(span, "langfuse.observation.type"),
            Some("generation")
        );
        assert_eq!(attribute(span, "langfuse.session.id"), Some("ingest-7"));
        assert_eq!(
            attribute(span, "langfuse.observation.model.name"),
            Some("test-model")
        );
        assert_eq!(
            attribute(span, "langfuse.observation.model.parameters"),
            Some(r#"{"temperature":0.2}"#)
        );
        assert_eq!(
            attribute(span, "langfuse.observation.input"),
            Some("Explain these bills")
        );
        assert_eq!(attribute(span, "langfuse.observation.output"), Some("[]"));
        let usage: Value = serde_json::from_str(
            attribute(span, "langfuse.observation.usage_details").expect("usage"),
        )
        .expect("usage is a JSON string");
        assert_eq!(
            usage,
            json!({ "input": 90, "output": 40, "input_cached_tokens": 10 })
        );
        assert_eq!(
            attribute(span, "langfuse.trace.metadata.bills"),
            Some("r1,r2")
        );
        assert_eq!(
            attribute(span, "langfuse.observation.metadata.bills"),
            Some("r1,r2")
        );
        assert!(attribute(span, "langfuse.observation.level").is_none());
    }

    #[test]
    fn a_failed_call_is_an_error_and_content_can_be_kept_out() {
        let on = vars(&[
            ("LANGFUSE_PUBLIC_KEY", "pk"),
            ("LANGFUSE_SECRET_KEY", "sk"),
            ("LANGFUSE_RECORD_CONTENT", "off"),
        ])
        .expect("on");
        let payload = on.payload(&call(Err("gemini: empty response".to_string()), &[]));
        let span = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["status"]["code"], 2);
        assert_eq!(span["status"]["message"], "gemini: empty response");
        assert_eq!(attribute(span, "langfuse.observation.level"), Some("ERROR"));
        assert_eq!(
            attribute(span, "langfuse.observation.status_message"),
            Some("gemini: empty response")
        );
        assert!(attribute(span, "langfuse.observation.input").is_none());
        assert!(attribute(span, "langfuse.observation.output").is_none());
        // The shape of the call still lands.
        assert!(attribute(span, "langfuse.observation.usage_details").is_some());
    }

    #[test]
    fn gemini_usage_splits_cached_and_thinking_tokens_into_their_own_buckets() {
        let usage = Usage::from_gemini(&json!({ "usageMetadata": {
            "promptTokenCount": 1000, "cachedContentTokenCount": 200,
            "candidatesTokenCount": 300, "thoughtsTokenCount": 50, "totalTokenCount": 1350
        } }))
        .expect("usage");
        assert_eq!(
            usage,
            Usage {
                input: 800,
                input_cached: 200,
                output: 300,
                output_reasoning: 50
            }
        );
        assert_eq!(
            usage.details(),
            json!({ "input": 800, "input_cached_tokens": 200, "output": 300,
                    "output_reasoning_tokens": 50 })
        );
        assert!(Usage::from_gemini(&json!({ "candidates": [] })).is_none());
    }
}
