use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use spar::{CompileOptions, Engine};

fn engine() -> Engine {
    Engine::new(CompileOptions::default())
}

#[test]
fn collections_and_option_fields_work_together_in_canonical_source() {
    let outcome = engine()
        .execute_source(
            r#"
            struct User {
                name: str = "Mike";
                nickname: Option<str> = none();
            };

            fn main() -> int {
                var mut numbers: List<int> = [1, 3];
                numbers.insert(index: 1, value: 2);
                numbers.append(value: 4);

                var mut scores: Map<str, int> = {};
                scores.insert(key: "mike", value: numbers.length());

                var user: User = User();
                if !user.nickname.isNone() { return 90; }
                return scores.get(key: "mike").unwrap();
            };
            "#,
        )
        .expect("canonical collections/Option program should execute");

    assert_eq!(outcome.exit_status, 4);
}

#[test]
fn typed_json_and_http_share_recursive_named_struct_decoding() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request);
        let body = r#"[{"tagName":"v1","author":{"login":"obi"}},{"tagName":"v2","author":{"login":"ada"}}]"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });

    let source = format!(
        r#"
        import pkg {{ get, HttpResponse }} from "std/http";
        import pkg {{ parse }} from "std/json";

        struct Author {{
            login: str = "";
        }};

        struct Release {{
            tagName: str = "";
            author: Author = Author();
        }};

        async fn main() -> int {{
            var direct: Release = parse<Release>(
                text: "{{\"tagName\":\"direct\",\"author\":{{\"login\":\"spar\"}}}}"
            );
            if direct.author.login != "spar" {{ return 91; }}

            var response: HttpResponse = await get(url: "http://{address}/releases");
            var releases: List<Release> = response.json<List<Release>>();
            if releases.length() != 2 {{ return 92; }}
            if releases.get(index: 1).unwrap().author.login != "ada" {{ return 93; }}
            return 0;
        }};
        "#
    );

    let outcome = engine()
        .execute_source(&source)
        .expect("typed JSON and HttpResponse.json<T>() should execute through the same contract");
    server.join().unwrap();

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn as_args_expands_each_list_element_to_one_exact_argv_entry() {
    let outcome = engine()
        .execute_source(
            r#"
            fn main() -> __shell {
                var excludes: List<str> = ["-x", "name with spaces", "*.literal"];
                return __shell {
                    sh -c 'test "$#" -eq 3 && test "$1" = "-x" && test "$2" = "name with spaces" && test "$3" = "*.literal"' marker ${excludes.asArgs()};
                };
            };
            "#,
        )
        .expect("Args expansion should preserve argv boundaries");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn named_structs_are_required_for_typed_structured_values() {
    let errors = engine()
        .check_source(
            r#"
            struct Address { city: str = "Awka"; };

            fn address() -> Address {
                return { city: "Awka"; };
            };
            "#,
        )
        .expect_err("anonymous object literals must not construct named structured values");

    let rendered = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(
        rendered.contains("Address") || rendered.contains("Record") || rendered.contains("object"),
        "{rendered}"
    );
}
