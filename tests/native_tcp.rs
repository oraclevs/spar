//! A Spar program accepts a TCP connection through an ABI 1 native module and serves HTTP.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use spar::{native_module, CompileOptions, Engine};

fn tcp_module() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../spar-native-sys/examples/native/rust-tcp");
    let output = Command::new("cargo")
        .args(["build", "--release", "--offline"])
        .current_dir(&root)
        .output()
        .expect("build native TCP module");
    assert!(
        output.status.success(),
        "native TCP build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    root.join("target/release").join(format!(
        "{}rust_tcp{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ))
}

#[test]
fn spar_serves_one_http_response_over_native_tcp() {
    let module = tcp_module();
    let mut natives = CompileOptions::default().natives;
    let info = native_module::load_into_registry(&module, &mut natives).expect("load TCP module");
    assert_eq!(info.abi_major, 1);
    assert_eq!(info.name, "nativeTcp");

    let port_probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = port_probe.local_addr().unwrap();
    drop(port_probe);
    let client = std::thread::spawn(move || {
        let mut stream = (0..40)
            .find_map(|_| match TcpStream::connect(address) {
                Ok(stream) => Some(stream),
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(25));
                    None
                }
            })
            .expect("Spar TCP server did not listen");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });

    let source = format!(
        r#"
        import nativeTcp;
        fn main() -> int {{
            var listener: TcpListenerHandle = nativeTcp.listen(address: "{address}");
            var connection: TcpConnectionHandle = nativeTcp.accept(listener: listener);
            nativeTcp.setReadTimeout(connection: connection, millis: 2000);
            var request: Bytes = nativeTcp.read(connection: connection, maxBytes: 4096);
            if request.length() == 0 {{ return 1; }}
            nativeTcp.writeText(connection: connection, text: "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
            nativeTcp.close(connection: connection);
            return 0;
        }};
        "#
    );
    let outcome = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .execute_source(&source)
    .expect("Spar TCP server should run");
    assert_eq!(outcome.exit_status, 0);
    let response = client.join().unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.ends_with("\r\n\r\nOK"), "{response}");
}

#[test]
fn local_tcp_package_loads_native_module_from_path_dependency() {
    let module = tcp_module();
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("app");
    let package = temp.path().join("tcp");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(package.join("src")).unwrap();
    let key = spar::package::native::host_target_keys()[0].clone();
    let artifact = format!(
        "native/{key}/{}rust_tcp{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let target = package.join(&artifact);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(module, &target).unwrap();
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../spar-native-sys/examples/native/rust-tcp/package/src/lib.spar");
    std::fs::copy(example, package.join("src/lib.spar")).unwrap();
    std::fs::write(
        package.join("spar.package.spar"),
        format!(
            "struct Package {{ name: str = \"tcp\"; version: str = \"0.1.0\"; kind: str = \"library\"; entry: str = \"src/lib.spar\"; }};\nstruct Native {{ module: str = \"nativeTcp\"; abi: str = \"spar-native-1\"; capabilities: str = \"strings,bytes,typed-arrays,resources\"; {key}: str = \"{artifact}\"; }};\n"
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("spar.package.spar"),
        "struct Package { name: str = \"app\"; version: str = \"0.1.0\"; kind: str = \"application\"; entry: str = \"src/main.spar\"; };\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/main.spar"),
        "import pkg { listen, localAddress } from \"tcp\";\nfn main() -> int { var listener = listen(address: \"127.0.0.1:0\"); var address = localAddress(listener: listener); if address.length() > 0 { return 0; } return 1; };\n",
    )
    .unwrap();
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let add = Command::new(env!("CARGO_BIN_EXE_spar"))
        .current_dir(&project)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", &cache)
        .args(["add", "tcp", &format!("path:{}", package.display())])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let run = Command::new(env!("CARGO_BIN_EXE_spar"))
        .current_dir(&project)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", &cache)
        .args(["exec", "src/main.spar"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
