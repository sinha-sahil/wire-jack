use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine;
use reqwest::header::{CONTENT_TYPE, COOKIE};
use reqwest::{Client, Method, RequestBuilder};

use crate::error::Result;
use crate::request::ApiRequest;
use crate::response::ApiResponse;

pub async fn execute(req: &ApiRequest) -> Result<ApiResponse> {
    let client = build_client(req)?;
    let max_attempts = req
        .retry
        .as_ref()
        .map(|r| r.max_attempts.max(1))
        .unwrap_or(1);

    let mut attempt = 0;
    loop {
        attempt += 1;
        let start = Instant::now();
        match build_request(&client, req).await?.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if attempt < max_attempts && should_retry_status(req, status) {
                    tokio::time::sleep(backoff_delay(req, attempt)).await;
                    continue;
                }
                return build_response(resp, start).await;
            }
            Err(e) => {
                if attempt < max_attempts {
                    tokio::time::sleep(backoff_delay(req, attempt)).await;
                    continue;
                }
                return Err(e.into());
            }
        }
    }
}

fn build_client(req: &ApiRequest) -> Result<Client> {
    let mut cb = Client::builder();
    if req.insecure == Some(true) {
        cb = cb.danger_accept_invalid_certs(true);
    }
    if req.follow_redirects == Some(true) {
        let max = req.max_redirects.unwrap_or(10) as usize;
        cb = cb.redirect(reqwest::redirect::Policy::limited(max));
    } else {
        cb = cb.redirect(reqwest::redirect::Policy::none());
    }
    if let Some(ms) = req.timeout_ms {
        cb = cb.timeout(Duration::from_millis(ms));
    }
    if let Some(proxy) = &req.proxy {
        cb = cb.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(cb.build()?)
}

async fn build_request(client: &Client, req: &ApiRequest) -> Result<RequestBuilder> {
    let method =
        Method::from_bytes(req.method.to_ascii_uppercase().as_bytes()).unwrap_or(Method::GET);
    let mut builder = client.request(method, &req.url);

    if !req.query.is_empty() {
        let pairs: Vec<(&str, &str)> = req
            .query
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        builder = builder.query(&pairs);
    }

    let explicit_ct = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone());

    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("content-type") {
            continue;
        }
        builder = builder.header(k, v);
    }

    if !req.cookies.is_empty() {
        let jar = req
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        builder = builder.header(COOKIE, jar);
    }

    if let Some(auth) = &req.auth {
        if auth.scheme.eq_ignore_ascii_case("bearer") {
            if let Some(token) = &auth.token {
                builder = builder.bearer_auth(token);
            }
        } else if auth.scheme.eq_ignore_ascii_case("basic") {
            builder = builder.basic_auth(
                auth.username.clone().unwrap_or_default(),
                auth.password.clone(),
            );
        }
    }

    if let Some(body) = &req.body {
        let media = explicit_ct.unwrap_or_else(|| "application/json".to_string());
        let base = media
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "application/x-www-form-urlencoded" => {
                builder = builder.form(&form_pairs(body));
            }
            "multipart/form-data" => {
                builder = builder.multipart(build_multipart(body).await?);
            }
            "application/json" => {
                builder = builder
                    .header(CONTENT_TYPE, media)
                    .body(serde_json::to_vec(body).unwrap_or_default());
            }
            b if is_binary(b) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(body.as_str().unwrap_or_default())
                    .unwrap_or_default();
                builder = builder.header(CONTENT_TYPE, media).body(decoded);
            }
            _ => {
                let data = match body {
                    serde_json::Value::String(s) => s.clone().into_bytes(),
                    other => serde_json::to_vec(other).unwrap_or_default(),
                };
                builder = builder.header(CONTENT_TYPE, media).body(data);
            }
        }
    } else if let Some(ct) = explicit_ct {
        builder = builder.header(CONTENT_TYPE, ct);
    }

    Ok(builder)
}

async fn build_response(resp: reqwest::Response, start: Instant) -> Result<ApiResponse> {
    let status = resp.status();
    let status_code = status.as_u16();
    let status_text = status.canonical_reason().unwrap_or("").to_string();
    let ok = status.is_success();

    let mut headers = BTreeMap::new();
    for (k, v) in resp.headers() {
        headers.insert(k.to_string(), v.to_str().unwrap_or("").to_string());
    }
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let content_length = resp.content_length();

    let mut cookies = BTreeMap::new();
    for cookie in resp.cookies() {
        cookies.insert(cookie.name().to_string(), cookie.value().to_string());
    }

    let bytes = resp.bytes().await?;
    let (body_text, body_json) = classify_body(content_type.as_deref(), &bytes);

    Ok(ApiResponse {
        status: status_code,
        status_text,
        ok,
        headers,
        content_type,
        content_length,
        cookies,
        body_text,
        body_json,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

fn classify_body(
    content_type: Option<&str>,
    bytes: &[u8],
) -> (Option<String>, Option<serde_json::Value>) {
    if bytes.is_empty() {
        return (None, None);
    }
    let media = content_type.map(|s| {
        s.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    });

    if media
        .as_deref()
        .is_some_and(|m| m == "application/json" || m == "text/json" || m.ends_with("+json"))
    {
        if let Ok(value) = serde_json::from_slice(bytes) {
            return (None, Some(value));
        }
        return (std::str::from_utf8(bytes).ok().map(|s| s.to_string()), None);
    }

    if is_textual(media.as_deref()) {
        return (std::str::from_utf8(bytes).ok().map(|s| s.to_string()), None);
    }

    (None, None)
}

fn is_textual(media: Option<&str>) -> bool {
    match media {
        Some(m) => {
            m.starts_with("text/")
                || m.ends_with("+xml")
                || matches!(
                    m,
                    "application/xml"
                        | "application/javascript"
                        | "application/x-www-form-urlencoded"
                )
        }
        None => false,
    }
}

fn is_binary(media: &str) -> bool {
    media == "application/octet-stream"
        || media == "application/pdf"
        || media == "application/zip"
        || media == "application/gzip"
        || media.starts_with("image/")
        || media.starts_with("audio/")
        || media.starts_with("video/")
        || media.starts_with("font/")
}

fn form_pairs(body: &serde_json::Value) -> Vec<(String, String)> {
    match body.as_object() {
        Some(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), json_to_string(v)))
            .collect(),
        None => Vec::new(),
    }
}

async fn build_multipart(body: &serde_json::Value) -> Result<reqwest::multipart::Form> {
    let mut form = reqwest::multipart::Form::new();
    if let Some(map) = body.as_object() {
        for (key, value) in map {
            if let Some(path) = value
                .as_object()
                .and_then(|o| o.get("$file"))
                .and_then(|p| p.as_str())
            {
                let bytes = tokio::fs::read(path).await?;
                let filename = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
                form = form.part(key.clone(), part);
            } else {
                form = form.text(key.clone(), json_to_string(value));
            }
        }
    }
    Ok(form)
}

fn json_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn should_retry_status(req: &ApiRequest, status: u16) -> bool {
    req.retry
        .as_ref()
        .is_some_and(|r| r.retry_on_status.contains(&status))
}

fn backoff_delay(req: &ApiRequest, attempt: u32) -> Duration {
    let Some(retry) = &req.retry else {
        return Duration::ZERO;
    };
    let base = retry.delay_ms.unwrap_or(0);
    let ms = if retry.backoff.eq_ignore_ascii_case("exponential") {
        base.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)))
    } else {
        base
    };
    let ms = retry.max_delay_ms.map_or(ms, |max| ms.min(max));
    Duration::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Retry;

    #[test]
    fn classifies_json() {
        let (text, json) = classify_body(Some("application/json; charset=utf-8"), br#"{"a":1}"#);
        assert!(text.is_none());
        assert_eq!(json.unwrap()["a"], serde_json::json!(1));
    }

    #[test]
    fn classifies_text() {
        let (text, json) = classify_body(Some("text/html"), b"<h1>hi</h1>");
        assert_eq!(text.unwrap(), "<h1>hi</h1>");
        assert!(json.is_none());
    }

    #[test]
    fn classifies_binary_and_empty_as_neither() {
        assert_eq!(
            classify_body(Some("application/octet-stream"), &[0, 159, 146]).0,
            None
        );
        assert_eq!(
            classify_body(Some("application/octet-stream"), &[0, 159, 146]).1,
            None
        );
        assert_eq!(classify_body(Some("application/json"), b"").1, None);
    }

    #[test]
    fn form_pairs_stringifies_values() {
        let pairs = form_pairs(&serde_json::json!({ "a": "x", "n": 5 }));
        assert!(pairs.contains(&("a".to_string(), "x".to_string())));
        assert!(pairs.contains(&("n".to_string(), "5".to_string())));
    }

    #[test]
    fn exponential_backoff_is_capped() {
        let req = ApiRequest {
            retry: Some(Retry {
                max_attempts: 5,
                backoff: "exponential".to_string(),
                delay_ms: Some(100),
                max_delay_ms: Some(350),
                retry_on_status: vec![],
            }),
            ..Default::default()
        };
        assert_eq!(backoff_delay(&req, 1), Duration::from_millis(100));
        assert_eq!(backoff_delay(&req, 2), Duration::from_millis(200));
        assert_eq!(backoff_delay(&req, 3), Duration::from_millis(350));
    }
}
