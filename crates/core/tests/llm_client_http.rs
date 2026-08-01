//! LlmClient wire-protocol tests against a local mock HTTP server (§5
//! Stage 6). Kills the network-path mutants: transport errors, HTTP status
//! mapping, decode failures, empty content, and the one-shot repair retry.
//!
//! The mock server is a raw std::net::TcpListener that speaks just enough
//! HTTP/1.1 to satisfy reqwest.

use std::io::{Read, Write};
use std::net::TcpListener;

use linkbot_core::error::PipelineError;
use linkbot_core::synthesizer::LlmClient;

/// Serve ONE request, capture its request line, respond with `body`.
/// Returns (port, request_line).
fn serve_once(status_line: &str, body: String) -> (u16, std::sync::Arc<std::sync::Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured: std::sync::Arc<std::sync::Mutex<String>> = std::sync::Arc::default();
    let cap2 = captured.clone();
    let status_line = status_line.to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            *cap2.lock().unwrap() = head;
            let resp = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (port, captured)
}

fn client(port: u16) -> LlmClient {
    LlmClient::new(
        format!("http://127.0.0.1:{port}"),
        "test-key".into(),
        "test-model".into(),
    )
}

fn ok_body(content: &str) -> String {
    format!(r#"{{"choices":[{{"message":{{"content":{content}}}}}]}}"#)
}

#[tokio::test]
async fn chat_json_success_extracts_content() {
    let (port, req) = serve_once("200 OK", ok_body(r#""{\"summary\": \"hi\"}""#));
    let c = client(port);
    let out = c.chat_json("sys", "usr").await.unwrap();
    assert_eq!(out, r#"{"summary": "hi"}"#);
    let head = req.lock().unwrap().clone();
    assert!(head.starts_with("POST /chat/completions "), "{head}");
}

#[tokio::test]
async fn chat_json_sends_auth_and_json_body() {
    let (port, req) = serve_once("200 OK", ok_body(r#""ok""#));
    let c = client(port);
    let _ = c.chat_json("sys", "usr").await.unwrap();
    let head = req.lock().unwrap().clone();
    assert!(head.to_lowercase().contains("authorization: bearer test-key"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");
}

#[tokio::test]
async fn chat_json_http_500_maps_to_synthesis_failed() {
    let (port, _) = serve_once("500 Internal Server Error", "{}".to_string());
    let c = client(port);
    let e = c.chat_json("sys", "usr").await.unwrap_err();
    assert!(matches!(e, PipelineError::SynthesisFailed(_)));
    assert!(e.to_string().contains("http 500"), "{e}");
}

#[tokio::test]
async fn chat_json_decode_failure_maps_cleanly() {
    let (port, _) = serve_once("200 OK", "this is not json".to_string());
    let c = client(port);
    let e = c.chat_json("sys", "usr").await.unwrap_err();
    assert!(matches!(e, PipelineError::SynthesisFailed(_)));
    assert!(e.to_string().contains("decode"), "{e}");
}

#[tokio::test]
async fn chat_json_empty_content_is_error() {
    // choices[].message.content missing → "empty llm content".
    let (port, _) = serve_once("200 OK", r#"{"choices":[{"message":{}}]}"#.to_string());
    let c = client(port);
    let e = c.chat_json("sys", "usr").await.unwrap_err();
    assert!(matches!(e, PipelineError::SynthesisFailed(_)));
    assert!(e.to_string().contains("empty"), "{e}");
}

#[tokio::test]
async fn chat_json_empty_choices_is_error() {
    let (port, _) = serve_once("200 OK", r#"{"choices":[]}"#.to_string());
    let c = client(port);
    assert!(c.chat_json("sys", "usr").await.is_err());
}

#[tokio::test]
async fn chat_json_transport_error_maps_cleanly() {
    // Connect to a port nothing listens on.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // port now closed
    let c = client(port);
    let e = c.chat_json("sys", "usr").await.unwrap_err();
    assert!(matches!(e, PipelineError::SynthesisFailed(_)));
    assert!(e.to_string().contains("transport"), "{e}");
}

#[tokio::test]
async fn synthesize_repair_retry_on_bad_json() {
    // First response: prose-wrapped invalid; second: valid. The client must
    // retry once and succeed.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = calls.clone();
    std::thread::spawn(move || {
        for i in 0..2 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = if i == 0 {
                    ok_body(r#""not valid json at all""#)
                } else {
                    ok_body(r#""{\"summary\":\"ok\",\"deep_analysis\":\"d\",\"critique\":\"c\",\"citations\":[]}""#)
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });
    let c = client(port);
    let src = linkbot_core::fetcher::FetchedArticle {
        url: "https://src.example/1".into(),
        title: "T".into(),
        published_date: None,
        author: None,
        language: None,
        text: "body".into(),
    };
    let s = c.synthesize(&src, &[]).await.unwrap();
    assert_eq!(s.summary, "ok");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "retried once");
}

#[tokio::test]
async fn synthesize_fails_cleanly_after_repair() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for _ in 0..2 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = ok_body(r#""still not json""#);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });
    let c = client(port);
    let src = linkbot_core::fetcher::FetchedArticle {
        url: "https://src.example/1".into(),
        title: "T".into(),
        published_date: None,
        author: None,
        language: None,
        text: "body".into(),
    };
    let e = c.synthesize(&src, &[]).await.unwrap_err();
    assert!(matches!(e, PipelineError::SynthesisFailed(_)));
    assert!(e.to_string().contains("json parse"), "{e}");
}

#[tokio::test]
async fn synthesize_success_no_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = calls.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = ok_body(r#""{\"summary\":\"s\",\"deep_analysis\":\"d\",\"critique\":\"c\",\"citations\":[{\"url\":\"https://x/1\",\"context\":\"ctx\"}]}""#);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    let c = client(port);
    let src = linkbot_core::fetcher::FetchedArticle {
        url: "https://src.example/1".into(),
        title: "T".into(),
        published_date: None,
        author: None,
        language: None,
        text: "body".into(),
    };
    let s = c.synthesize(&src, &[]).await.unwrap();
    assert_eq!(s.citations.len(), 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "no retry on success");
}

#[tokio::test]
async fn synthesize_tolerates_duplicate_json_keys() {
    // Real-world failure: the model emitted the same field twice and serde's
    // struct parser rejected it hard ("duplicate field `deep_analysis`").
    // Value-first parsing must accept it (last-wins) without a repair retry.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = calls.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = ok_body(
                r#""{\"summary\":\"s\",\"deep_analysis\":\"first\",\"deep_analysis\":\"second\",\"critique\":\"c\",\"citations\":[]}""#,
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    let c = client(port);
    let src = linkbot_core::fetcher::FetchedArticle {
        url: "https://src.example/1".into(),
        title: "T".into(),
        published_date: None,
        author: None,
        language: None,
        text: "body".into(),
    };
    let s = c.synthesize(&src, &[]).await.unwrap();
    assert_eq!(s.deep_analysis, "second", "last-wins on duplicate keys");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "no retry needed");
}
