use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use spar::{CompileOptions, Engine};

#[test]
fn http_get_works_against_loopback_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npong")
            .unwrap();
    });

    let source = format!(
        r#"
        import pkg {{ get }} from "std/http";
        async function main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/health");
            if response.status != 200 {{ return 1; }}
            if response.body != "pong" {{ return 2; }}
            return 0;
        }};
        "#
    );

    let outcome = Engine::new(CompileOptions::default())
        .execute_source(&source)
        .expect("http std module should execute");
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 0);
}
