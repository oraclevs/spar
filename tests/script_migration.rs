use spar::{CompileOptions, Engine};

fn run(source: &str) -> i32 {
    Engine::new(CompileOptions::default())
        .execute_source(source)
        .unwrap_or_else(|errors| panic!("{errors:?}"))
        .exit_status
}

#[test]
fn contextual_keyword_is_a_named_constructor_argument() {
    assert_eq!(
        run(r#"
        struct Alias { command: List<str> = []; };
        fn main() -> int {
            var alias = Alias(command: ["git", "status"]);
            return alias.command.length();
        };
    "#),
        2
    );
}

#[test]
fn named_closure_call_resolves_local_callable() {
    assert_eq!(
        run(r#"
        fn main() -> int {
            var minimum: int = 21;
            var adult: fn(age: int) -> bool = |age: int| age >= minimum;
            if adult(age: 24) { return 0; }
            return 1;
        };
    "#),
        0
    );
}

#[test]
fn map_default_has_the_declared_parameter_type() {
    assert_eq!(
        run(r#"
        fn count(headers: Map<str, str> = {}) -> int { return headers.length(); };
        fn main() -> int { return count(); };
    "#),
        0
    );
}

#[test]
fn http_get_wrapper_awaits_its_request() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!(
        r#"
        import pkg {{ get, Duration, HttpResponse }} from "std/http";
        async fn main() -> int {{
            var response: HttpResponse = await get(url: "http://{address}/");
            return response.status;
        }};
    "#
    );
    let engine = Engine::default();
    let program = engine.compile_source(&source).unwrap();
    let server = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        connection
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        connection.read(&mut request).unwrap();
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
    });
    let outcome = engine.execute_compiled(&program).unwrap();
    server.join().unwrap();
    assert_eq!(outcome.exit_status, 200);
}

#[test]
fn dotenv_reaches_compiled_native_calls_without_overriding_context() {
    let directory = tempfile::tempdir().unwrap();
    let key = "SPAR_MIGRATION_DOTENV_PROBE";
    std::fs::write(directory.path().join(".env"), format!("{key}=from-file\n")).unwrap();
    let engine = Engine::default().with_base_dir(directory.path());
    let source = format!(
        r#"
        @LoadEnv
        import pkg {{ get, has }} from "std/env";
        fn main() -> int {{
            if !has(name: "{key}") {{ return 1; }}
            if get(name: "{key}") == "from-file" {{ return 2; }}
            if get(name: "{key}") == "from-context" {{ return 3; }}
            return 4;
        }};
    "#
    );
    let program = engine.compile_source(&source).unwrap();
    let mut context = spar::RuntimeContext::for_base_dir(directory.path());
    context.env_unset(key);
    assert_eq!(
        engine
            .execute_compiled_with_context(&program, context)
            .unwrap()
            .exit_status,
        2
    );
    let mut context = spar::RuntimeContext::for_base_dir(directory.path());
    context.env_set(key, "from-context");
    assert_eq!(
        engine
            .execute_compiled_with_context(&program, context)
            .unwrap()
            .exit_status,
        3
    );
    assert!(std::env::var_os(key).is_none());
}
