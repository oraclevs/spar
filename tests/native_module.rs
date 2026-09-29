//! End-to-end tests for the native module ABI: a C module is compiled with the system C compiler
//! against the public header only, loaded, registered, and called from real Spar source.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use spar::native_module;
use spar::{CompileOptions, Engine, NativeRegistry};

fn sys_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spar-native-sys")
}

/// Compiles the C example once per test process.
fn c_fastmath() -> &'static Path {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("spar-native-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lib = dir.join("libfastmath.so");
        let sys = sys_dir();
        let out = Command::new("cc")
            .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fPIC", "-fvisibility=hidden", "-shared"])
            .arg(format!("-I{}", sys.join("include").display()))
            .arg(sys.join("examples/native/c-fastmath/fastmath.c"))
            .args(["-lm", "-o"])
            .arg(&lib)
            .output()
            .expect("cc");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        lib
    })
}

fn engine_with_native(path: &Path) -> Engine {
    let mut natives = spar::CompileOptions::default().natives;
    native_module::load_into_registry(path, &mut natives).expect("load native module");
    Engine::new(CompileOptions { natives, ..CompileOptions::default() })
}

fn run_int(source: &str) -> i32 {
    engine_with_native(c_fastmath()).execute_source(source).expect("program runs").exit_status
}

fn run_err(source: &str) -> String {
    match engine_with_native(c_fastmath()).execute_source(source) {
        Ok(o) => panic!("expected failure, exit {}", o.exit_status),
        Err(errors) => errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"),
    }
}

#[test]
fn module_loads_and_reports_info() {
    let info = native_module::load_module(c_fastmath()).unwrap().info().clone();
    assert_eq!(info.name, "fastMath");
    assert_eq!(info.version, (0, 1, 0));
    assert!(info.functions.contains(&"add".to_string()));
    let mut reg = NativeRegistry::new();
    let again = native_module::load_into_registry(c_fastmath(), &mut reg).unwrap();
    assert_eq!(again.name, "fastMath");
}

#[test]
fn scalar_calls() {
    assert_eq!(run_int("function main() -> int { return fastMath::add(a: 20, b: 22); };"), 42);
    assert_eq!(run_int("function main() -> int { if fastMath::hypot(x: 3.0, y: 4.0) == 5.0 { return 1; } return 0; };"), 1);
}

#[test]
fn strings_are_borrowed_and_created() {
    assert_eq!(run_int(r#"function main() -> int { return fastMath::strLen(text: "héllo"); };"#), 6);
    assert_eq!(
        run_int(r#"function main() -> int { if fastMath::shout(text: "abc") == "ABC!" { return 7; } return 0; };"#),
        7
    );
}

#[test]
fn typed_list_borrow_copies_into_contiguous_buffer() {
    assert_eq!(
        run_int("function main() -> int { if fastMath::sumF64(values: [1.5, 2.5, 6.0]) == 10.0 { return 1; } return 0; };"),
        1
    );
}

#[test]
fn native_built_list_and_record_round_trip() {
    assert_eq!(run_int("function main() -> int { return len(value: fastMath::iota(n: 5)); };"), 5);
    assert_eq!(run_int("function main() -> int { var p = fastMath::pair(a: 3, b: 4); return p.sum.asInt() * 100 + p.product.asInt(); };"), 712);
}

#[test]
fn native_errors_carry_message_and_span() {
    let msg = run_err("function main() -> int { return fastMath::fail(); };");
    assert!(msg.contains("deliberate failure from C"), "{msg}");
    let msg = run_err("function main() -> int { return fastMath::add(a: 9223372036854775807, b: 1); };");
    assert!(msg.contains("add overflows int"), "{msg}");
}

#[test]
fn signature_is_typechecked_from_descriptor() {
    let engine = engine_with_native(c_fastmath());
    let errors = engine
        .check_source(r#"function main() -> int { return fastMath::add(a: "x", b: 1); };"#)
        .expect_err("type error expected");
    let text = errors.iter().map(|e| e.to_string()).collect::<String>();
    assert!(text.contains("expects int"), "{text}");
}

#[test]
fn rejects_non_module_and_missing_files() {
    let err = native_module::load_module(Path::new("/nonexistent/lib.so")).unwrap_err().to_string();
    assert!(err.contains("cannot load native module"), "{err}");
    // libm exists everywhere but does not export the Spar entry symbol.
    let libm = ["/usr/lib/libm.so.6", "/lib/x86_64-linux-gnu/libm.so.6", "/usr/lib64/libm.so.6"]
        .iter()
        .map(Path::new)
        .find(|p| p.exists());
    if let Some(libm) = libm {
        let err = native_module::load_module(libm).unwrap_err().to_string();
        assert!(err.contains("does not export"), "{err}");
    }
}
