use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration as StdDuration;

use spar::{CompileOptions, Engine};

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(StdDuration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
            Err(error) => panic!("request read failed: {error}"),
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn serve_and_capture(
    listener: TcpListener,
    status: &'static str,
    body: &'static str,
    response_headers: &'static [(&'static str, &'static str)],
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let headers = response_headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        let response = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        request
    })
}

#[test]
fn http_client_applies_base_url_default_headers_query_request_headers_and_text_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ HttpClient, Duration, HttpResponse }} from "std/http";

        async fn main() -> int {{
            var client: HttpClient = HttpClient(
                baseUrl: "http://{address}",
                headers: {{ Accept: "application/json"; }},
                timeout: Duration(seconds: 2, millis: 0),
            );
            var response: HttpResponse = await client.post(
                path: "/echo",
                query: {{ page: "2"; q: "spar core"; }},
                headers: {{ Authorization: "Bearer test"; }},
                body: "hello",
            );
            if response.status != 201 {{ return 1; }}
            return 0;
        }};
        "#
    );

    let engine = Engine::new(CompileOptions::default());
    let program = engine.compile_source(&source).expect("HttpClient request should compile");
    let server = serve_and_capture(listener, "201 Created", "ok", &[]);
    let outcome = engine.execute_compiled(&program).expect("HttpClient request should execute");
    let request = server.join().unwrap();

    assert_eq!(outcome.exit_status, 0);
    assert!(request.starts_with("POST /echo?"), "{request}");
    assert!(request.contains("page=2"), "{request}");
    assert!(request.contains("q=spar%20core") || request.contains("q=spar+core"), "{request}");
    assert!(request.contains("Accept: application/json"), "{request}");
    assert!(request.contains("Authorization: Bearer test"), "{request}");
    assert!(request.ends_with("hello"), "{request}");
}

#[test]
fn http_client_serializes_native_json_body_and_sets_content_type() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ post, HttpResponse }} from "std/http";

        async fn main() -> int {{
            var response: HttpResponse = await post(
                url: "http://{address}/json",
                jsonBody: {{ name: "spar"; count: 2; }},
            );
            if response.status == 200 {{ return 0; }}
            return 1;
        }};
        "#
    );

    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(&source)
        .expect("JSON request body should compile from native Spar values");
    let server = serve_and_capture(listener, "200 OK", "ok", &[]);
    let outcome = engine
        .execute_compiled(&program)
        .expect("JSON request body should serialize and send");
    let request = server.join().unwrap();

    assert_eq!(outcome.exit_status, 0);
    assert!(
        request.to_ascii_lowercase().contains("content-type: application/json"),
        "{request}"
    );
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let json: serde_json::Value = serde_json::from_str(body).expect("request body must be JSON");
    assert_eq!(json["name"], "spar");
    assert_eq!(json["count"], 2);
}

#[test]
fn http_response_exposes_headers_optional_content_type_and_non_success_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ get, HttpResponse }} from "std/http";

        async fn main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/missing");
            if response.status != 404 {{ return 1; }}
            if !response.isClientError() {{ return 2; }}
            if response.body != "missing" {{ return 3; }}
            if response.contentType.unwrap() != "application/json" {{ return 4; }}
            // response.headers keys are always lowercased (ureq's own
            // headers_names() lowercases them and doesn't expose the
            // original casing — the same convention Node's
            // http.IncomingMessage.headers uses), regardless of how the
            // server capitalized them on the wire.
            if response.headers.get(key: "x-request-id").unwrap() != "abc123" {{ return 5; }}
            return 0;
        }};
        "#
    );

    let engine = Engine::new(CompileOptions::default());
    let program = engine.compile_source(&source).expect("inspectable error response should compile");
    let server = serve_and_capture(
        listener,
        "404 Not Found",
        "missing",
        &[("Content-Type", "application/json"), ("X-Request-Id", "abc123")],
    );
    let outcome = engine.execute_compiled(&program).expect("404 must remain an HttpResponse value");
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn http_response_json_decodes_array_roots_and_nested_structs() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ get, HttpResponse }} from "std/http";

        struct Author {{ id: int = 0; login: str = ""; }};
        struct Release {{ tagName: str = ""; author: Author = Author(); }};

        async fn main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/releases");
            var releases: List<Release> = response.json<List<Release>>();
            if releases.length() != 2 {{ return 1; }}
            if releases.get(index: 1).unwrap().author.login != "ada" {{ return 2; }}
            return 0;
        }};
        "#
    );

    let engine = Engine::new(CompileOptions::default());
    let program = engine.compile_source(&source).expect("generic HttpResponse.json<T>() should compile");
    let server = serve_and_capture(
        listener,
        "200 OK",
        r#"[{"tagName":"v1","author":{"id":1,"login":"obi"}},{"tagName":"v2","author":{"id":2,"login":"ada"}}]"#,
        &[("Content-Type", "application/json")],
    );
    let outcome = engine.execute_compiled(&program).expect("typed response JSON should execute");
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn http_client_timeout_is_a_transport_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ HttpClient, Duration, HttpResponse }} from "std/http";

        async fn main() -> int {{
            var client: HttpClient = HttpClient(
                baseUrl: "http://{address}",
                timeout: Duration(seconds: 0, millis: 20),
            );
            var response: HttpResponse = await client.get(path: "/slow");
            return response.status;
        }};
        "#
    );

    let engine = Engine::new(CompileOptions::default());
    let program = engine.compile_source(&source).expect("timeout client should compile");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_request(&mut stream);
        thread::sleep(StdDuration::from_millis(100));
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    });

    let errors = engine.execute_compiled(&program).expect_err("timeout should surface as transport failure");
    server.join().unwrap();
    let rendered = format!("{errors:?}");
    assert!(
        rendered.contains("HTTP transport failed")
            || rendered.contains("timed out")
            || rendered.contains("timeout"),
        "{rendered}"
    );
}

#[test]
fn http_response_is_a_concrete_struct_value() {
    let source = r#"
        import pkg { HttpResponse } from "std/http";

        fn main() -> int {
            var response: HttpResponse = HttpResponse();
            if response.status != 0 { return 1; }
            if !response.contentType.isNone() { return 2; }
            return 0;
        };
    "#;

    let outcome = Engine::new(CompileOptions::default())
        .execute_source(source)
        .expect("HttpResponse should be a constructible struct");
    assert_eq!(outcome.exit_status, 0);
}
