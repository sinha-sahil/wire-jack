use serde_json::json;
use wire_jack::{
    execute, render_request, render_response, run, RunResult, Template, Value, WireJackError,
};

mod common;

fn compile(src: &str) -> Template {
    Template::compile(src).unwrap_or_else(|e| {
        panic!(
            "{}",
            e.iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
}

async fn run_full(src: &str, data: Value) -> RunResult {
    run(&compile(src), data).await.unwrap()
}

async fn run_contract(src: &str, data: Value) -> serde_json::Value {
    run_full(src, data).await.output
}

fn base(addr: std::net::SocketAddr, pairs: Vec<(&str, Value)>) -> Value {
    let mut all = vec![("base", Value::Str(format!("http://{addr}").into()))];
    all.extend(pairs);
    Value::obj(all)
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

#[tokio::test]
async fn get_with_query_params() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":    concat(input.request.base, "/items"),
              "method": "GET",
              "query":  { "a": "1", "b": "two" }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    let echo = &out["body_json"];

    assert_eq!(echo["method"], "GET");
    assert_eq!(echo["path"], "/items");
    assert_eq!(echo["query"], "a=1&b=two");
}

#[tokio::test]
async fn post_json_body() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/users"),
              "method":  "POST",
              "headers": { "Content-Type": "application/json" },
              "body":    { "name": "Ada", "age": 30, "active": true }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    let echo = &out["body_json"];

    assert_eq!(echo["method"], "POST");
    assert!(echo["headers"]["content-type"]
        .as_str()
        .unwrap()
        .starts_with("application/json"));
    let body: serde_json::Value = serde_json::from_str(echo["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({ "name": "Ada", "age": 30, "active": true }));
}

#[tokio::test]
async fn post_form_urlencoded_body() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/form"),
              "method":  "POST",
              "headers": { "Content-Type": "application/x-www-form-urlencoded" },
              "body":    { "name": "Ada", "age": 30 }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    let echo = &out["body_json"];

    assert_eq!(echo["method"], "POST");
    assert!(echo["headers"]["content-type"]
        .as_str()
        .unwrap()
        .starts_with("application/x-www-form-urlencoded"));
    assert_eq!(echo["body"], "name=Ada&age=30");
}

#[tokio::test]
async fn put_patch_delete_methods() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/r/1"),
              "method":  input.request.method,
              "headers": { "Content-Type": "application/json" },
              "body":    { "v": 1 }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    for method in ["PUT", "PATCH", "DELETE"] {
        let out = run_contract(src, base(addr, vec![("method", s(method))])).await;
        assert_eq!(out["body_json"]["method"], method);
    }
}

#[tokio::test]
async fn headers_and_bearer_auth() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/secure"),
              "method":  "GET",
              "headers": { "X-Custom": "abc" },
              "auth":    { "scheme": "bearer", "token": "tok123" }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    let echo = &out["body_json"];

    assert_eq!(echo["headers"]["x-custom"], "abc");
    assert_eq!(echo["headers"]["authorization"], "Bearer tok123");
}

#[tokio::test]
async fn data_casting_in_body() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/cast"),
              "method":  "POST",
              "headers": { "Content-Type": "application/json" },
              "body": {
                  "id":      input.request.id,
                  "qty":     to_number(input.request.qty),
                  "price":   to_number(input.request.price),
                  "id_str":  to_string(input.request.id),
                  "active":  input.request.flag,
                  "encoded": json_encode({ "a": 1 })
              }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let data = base(
        addr,
        vec![
            ("id", Value::Int(7)),
            ("qty", s("42")),
            ("price", s("19.99")),
            ("flag", Value::Bool(true)),
        ],
    );
    let out = run_contract(src, data).await;
    let echo = &out["body_json"];
    let body: serde_json::Value = serde_json::from_str(echo["body"].as_str().unwrap()).unwrap();

    assert_eq!(body["id"], json!(7)); // int passthrough -> number
    assert_eq!(body["qty"], json!(42)); // to_number("42") -> integer
    assert_eq!(body["id_str"], json!("7")); // to_string(7) -> string
    assert_eq!(body["active"], json!(true));
    assert_eq!(body["encoded"], json!("{\"a\":1}")); // json_encode -> string
                                                     // temple Decimals bridge to serde_json as strings (exact, never f64).
    assert_eq!(body["price"], json!("19.99"));
}

#[tokio::test]
async fn multipart_form_with_file() {
    let file = std::env::temp_dir().join("wire_jack_mp.txt");
    std::fs::write(&file, "file-content").unwrap();
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/upload"),
              "method":  "POST",
              "headers": { "Content-Type": "multipart/form-data" },
              "body":    { "field": "hi", "doc": { "$file": input.request.file } }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![("file", s(&file.to_string_lossy()))])).await;
    let echo = &out["body_json"];

    assert!(echo["headers"]["content-type"]
        .as_str()
        .unwrap()
        .starts_with("multipart/form-data"));
    let body = echo["body"].as_str().unwrap();
    assert!(body.contains("name=\"field\"") && body.contains("hi"));
    assert!(body.contains("name=\"doc\"") && body.contains("file-content"));
}

#[tokio::test]
async fn response_body_classification() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, input.request.path), "method": "GET" }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;

    let json_out = run_contract(src, base(addr, vec![("path", s("/json"))])).await;
    assert!(json_out["body_json"].is_object());
    assert!(json_out["body_text"].is_null());

    let text_out = run_contract(src, base(addr, vec![("path", s("/text"))])).await;
    assert_eq!(text_out["body_text"], "hello world");
    assert!(text_out["body_json"].is_null());
}

#[tokio::test]
async fn request_scoped_vars_build_headers() {
    let src = r#"{{
        input.phase == "request"
          ? let path  = concat("/users/", to_string(input.request.id)) in
            let token = concat("Bearer ", input.request.token) in {
              "url":     concat(input.request.base, path),
              "method":  "GET",
              "headers": { "Authorization": token, "X-Trace": input.request.trace }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let data = base(
        addr,
        vec![
            ("id", Value::Int(7)),
            ("token", s("tok")),
            ("trace", s("trace-9")),
        ],
    );
    let out = run_contract(src, data).await;
    let echo = &out["body_json"];

    assert_eq!(echo["path"], "/users/7");
    assert_eq!(echo["headers"]["authorization"], "Bearer tok");
    assert_eq!(echo["headers"]["x-trace"], "trace-9");
}

#[tokio::test]
async fn response_phase_reshape_and_casting() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, "/echo"), "method": "GET" }
          : {
              "status_str":   to_string(input.response.status),
              "method_shout": upper(input.response.body_json.method),
              "ok":           input.response.ok
            }
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;

    assert_eq!(out["status_str"], "200");
    assert_eq!(out["method_shout"], "GET");
    assert_eq!(out["ok"], true);
}

#[tokio::test]
async fn response_phase_can_read_original_request_data() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":    concat(input.request.base, "/items"),
              "method": "GET",
              "query":  { "id": to_string(input.request.id) }
            }
          : {
              "requested_id": input.request.id,
              "requested_provider": input.request.provider,
              "echoed_query": input.response.body_json.query
            }
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(
        src,
        base(
            addr,
            vec![("id", Value::Int(42)), ("provider", s("EXPRESS"))],
        ),
    )
    .await;

    assert_eq!(out["requested_id"], json!(42));
    assert_eq!(out["requested_provider"], json!("EXPRESS"));
    assert_eq!(out["echoed_query"], json!("id=42"));
}

#[tokio::test]
async fn content_type_with_charset_is_preserved_exactly() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/json"),
              "method":  "POST",
              "headers": { "Content-Type": "application/json; charset=utf-8" },
              "body":    { "k": 1 }
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    assert_eq!(
        out["body_json"]["headers"]["content-type"],
        "application/json; charset=utf-8"
    );
}

#[tokio::test]
async fn body_without_content_type_defaults_to_json() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, "/x"), "method": "POST", "body": { "k": 1 } }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    let echo = &out["body_json"];
    assert!(echo["headers"]["content-type"]
        .as_str()
        .unwrap()
        .starts_with("application/json"));
    let body: serde_json::Value = serde_json::from_str(echo["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({ "k": 1 }));
}

#[tokio::test]
async fn raw_text_body_sent_verbatim() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/raw"),
              "method":  "POST",
              "headers": { "Content-Type": "text/plain" },
              "body":    "hello, plain text"
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    let echo = &out["body_json"];
    assert_eq!(echo["headers"]["content-type"], "text/plain");
    assert_eq!(echo["body"], "hello, plain text");
}

#[tokio::test]
async fn binary_octet_stream_body_is_base64_decoded() {
    let src = r#"{{
        input.phase == "request"
          ? {
              "url":     concat(input.request.base, "/bin"),
              "method":  "PUT",
              "headers": { "Content-Type": "application/octet-stream" },
              "body":    input.request.payload
            }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    // base64("hello") == "aGVsbG8="
    let out = run_contract(src, base(addr, vec![("payload", s("aGVsbG8="))])).await;
    let echo = &out["body_json"];
    assert_eq!(echo["method"], "PUT");
    assert_eq!(echo["headers"]["content-type"], "application/octet-stream");
    assert_eq!(echo["body"], "hello");
}

#[tokio::test]
async fn run_result_carries_raw_response_on_error_status() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, "/status/400"), "method": "GET" }
          : { "handled": input.response.ok }
    }}"#;
    let addr = common::spawn_echo().await;
    let result = run_full(src, base(addr, vec![])).await;

    assert_eq!(result.output["handled"], json!(false));
    assert_eq!(result.response.status, 400);
    assert!(!result.response.ok);
    assert_eq!(
        result.response.body_json,
        Some(json!({ "error": "simulated failure" }))
    );
}

#[tokio::test]
async fn run_result_carries_raw_response_on_success() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, "/echo"), "method": "GET" }
          : { "method": input.response.body_json.method }
    }}"#;
    let addr = common::spawn_echo().await;
    let result = run_full(src, base(addr, vec![])).await;

    assert_eq!(result.output["method"], "GET");
    assert_eq!(result.response.status, 200);
    assert!(result.response.ok);
}

#[tokio::test]
async fn render_error_on_error_status_names_the_status() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, "/status/503"), "method": "GET" }
          : { "id": input.response.body_json.id }
    }}"#;
    let addr = common::spawn_echo().await;
    let err = run(&compile(src), base(addr, vec![])).await.unwrap_err();

    match err {
        WireJackError::Render(msg) => {
            assert!(msg.contains("503"), "missing status in: {msg}");
        }
        other => panic!("expected render error, got: {other}"),
    }
}

#[tokio::test]
async fn phases_compose_around_a_status_check() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, input.request.path), "method": "GET" }
          : { "method": input.response.body_json.method }
    }}"#;
    let template = compile(src);
    let addr = common::spawn_echo().await;

    let refused = render_request(&template, base(addr, vec![("path", s("/status/401"))])).unwrap();
    let response = execute(&refused).await.unwrap();
    assert!(!response.ok);
    assert_eq!(response.status, 401);

    let data = base(addr, vec![("path", s("/echo"))]);
    let request = render_request(&template, data.clone()).unwrap();
    let response = execute(&request).await.unwrap();
    assert!(response.ok);
    let output = render_response(&template, data, &response).unwrap();
    assert_eq!(output["method"], "GET");
}

#[tokio::test]
async fn get_sends_no_content_type() {
    let src = r#"{{
        input.phase == "request"
          ? { "url": concat(input.request.base, "/g"), "method": "GET" }
          : input.response
    }}"#;
    let addr = common::spawn_echo().await;
    let out = run_contract(src, base(addr, vec![])).await;
    assert!(out["body_json"]["headers"]["content-type"].is_null());
}
