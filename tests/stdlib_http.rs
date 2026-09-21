use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use spar::{CompileOptions, Engine};

fn serve_once(listener: TcpListener, status_line: &str, body: &str, content_type: Option<&str>) {
    let (mut stream, _) = listener.accept().unwrap();
    let mut request = [0u8; 2048];
    let _ = stream.read(&mut request).unwrap();
    let content_type = content_type
        .map(|value| format!("Content-Type: {value}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status_line}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
}

#[test]
fn http_get_works_against_loopback_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ get }} from "std/http";
        async function main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/health");
            if response.status != 200 {{ return 1; }}
            if response.body != "pong" {{ return 2; }}
            if response.contentType != "" {{ return 3; }}
            return 0;
        }};
        "#
    );
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(&source)
        .expect("http std module should compile");
    let server = thread::spawn(move || serve_once(listener, "200 OK", "pong", None));

    let outcome = engine
        .execute_compiled(&program)
        .expect("http std module should execute");
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn http_response_methods_expose_text_json_and_success_status() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ get }} from "std/http";
        async function main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/user");
            if response.text() != "{{\"name\":\"Ada\",\"active\":true}}" {{ return 1; }}
            if response.isSuccess() != true {{ return 2; }}
            if response.isClientError() != false {{ return 3; }}
            if response.isServerError() != false {{ return 4; }}
            var payload: Record = response.json();
            if payload.name.asStr() != "Ada" {{ return 5; }}
            if payload.active.asBool() != true {{ return 6; }}
            if payload.name != "Ada" {{ return 7; }}
            if !payload.has("name") {{ return 8; }}
            return 0;
        }};
        "#
    );
    let engine = Engine::new(CompileOptions::default());
    let program = engine
        .compile_source(&source)
        .expect("HttpResponse methods should typecheck");
    let server = thread::spawn(move || {
        serve_once(
            listener,
            "200 OK",
            r#"{"name":"Ada","active":true}"#,
            Some("application/json"),
        )
    });

    let outcome = engine
        .execute_compiled(&program)
        .expect("HttpResponse methods should execute");
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn http_response_status_helpers_classify_client_and_server_errors() {
    fn run_status(status_line: &'static str, expected_method: &'static str) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let source = format!(
            r#"
            import pkg {{ get }} from "std/http";
            async function main() -> int {{
                var response: HttpResponse = await get(url: "http://{address}/status");
                if response.{expected_method}() != true {{ return 1; }}
                if response.isSuccess() != false {{ return 2; }}
                return 0;
            }};
            "#
        );
        let engine = Engine::new(CompileOptions::default());
        let program = engine
            .compile_source(&source)
            .expect("HttpResponse status helper should typecheck");
        let server = thread::spawn(move || serve_once(listener, status_line, "", None));

        let outcome = engine
            .execute_compiled(&program)
            .expect("HttpResponse status helper should execute");
        server.join().unwrap();
        assert_eq!(outcome.exit_status, 0);
    }

    run_status("404 Not Found", "isClientError");
    run_status("503 Service Unavailable", "isServerError");
}

#[test]
fn http_response_json_rejects_invalid_or_non_object_json() {
    for body in ["not json", "[1,2,3]"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let source = format!(
            r#"
            import pkg {{ get }} from "std/http";
            async function main() -> int {{
                var response: HttpResponse = await get(url: "http://{address}/json");
                var payload: Record = response.json();
                return 0;
            }};
            "#
        );
        let engine = Engine::new(CompileOptions::default());
        let program = engine
            .compile_source(&source)
            .expect("HttpResponse.json() should typecheck");
        let server_body = body.to_string();
        let server = thread::spawn(move || {
            serve_once(listener, "200 OK", &server_body, Some("application/json"))
        });

        let errors = engine
            .execute_compiled(&program)
            .expect_err("HttpResponse.json() must reject invalid/non-object JSON");
        server.join().unwrap();
        let rendered = format!("{errors:?}");
        assert!(
            rendered.contains("invalid JSON") || rendered.contains("JSON object"),
            "unexpected error: {rendered}"
        );
    }
}

#[test]
fn http_response_reports_the_content_type() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ get }} from "std/http";
        async function main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/page");
            if response.contentType != "text/html; charset=utf-8" {{ return 1; }}
            return 0;
        }};
        "#
    );
    let engine = Engine::new(CompileOptions::default());
    let program = engine.compile_source(&source).unwrap();
    let server = thread::spawn(move || {
        serve_once(
            listener,
            "200 OK",
            "<html></html>",
            Some("text/html; charset=utf-8"),
        )
    });

    let outcome = engine.execute_compiled(&program).unwrap();
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 0);
}
