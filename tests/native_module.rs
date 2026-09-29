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
            .args([
                "-std=c11",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fPIC",
                "-fvisibility=hidden",
                "-shared",
            ])
            .args(
                std::env::var("SPAR_TEST_CFLAGS")
                    .unwrap_or_default()
                    .split_whitespace(),
            )
            .arg(format!("-I{}", sys.join("include").display()))
            .arg(sys.join("examples/native/c-fastmath/fastmath.c"))
            .args(["-lm", "-lpthread", "-o"])
            .arg(&lib)
            .output()
            .expect("cc");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        lib
    })
}

fn engine_with_native(path: &Path) -> Engine {
    let mut natives = spar::CompileOptions::default().natives;
    native_module::load_into_registry(path, &mut natives).expect("load native module");
    Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
}

fn run_int(source: &str) -> i32 {
    engine_with_native(c_fastmath())
        .execute_source(source)
        .expect("program runs")
        .exit_status
}

fn run_err(source: &str) -> String {
    match engine_with_native(c_fastmath()).execute_source(source) {
        Ok(o) => panic!("expected failure, exit {}", o.exit_status),
        Err(errors) => errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[test]
fn module_loads_and_reports_info() {
    let info = native_module::load_module(c_fastmath())
        .unwrap()
        .info()
        .clone();
    assert_eq!(info.name, "fastMath");
    assert_eq!(info.version, (0, 1, 0));
    assert!(info.functions.contains(&"add".to_string()));
    let mut reg = NativeRegistry::new();
    let again = native_module::load_into_registry(c_fastmath(), &mut reg).unwrap();
    assert_eq!(again.name, "fastMath");
}

#[test]
fn scalar_calls() {
    assert_eq!(
        run_int("function main() -> int { return fastMath::add(a: 20, b: 22); };"),
        42
    );
    assert_eq!(run_int("function main() -> int { if fastMath::hypot(x: 3.0, y: 4.0) == 5.0 { return 1; } return 0; };"), 1);
}

#[test]
fn direct_signatures_skip_marshalling_and_are_typechecked() {
    assert_eq!(
        run_int("function main() -> int { return fastMath::mulD(a: 6, b: 7); };"),
        42
    );
    assert_eq!(
        run_int("function main() -> int { if fastMath::scaleD(x: 2.0, k: 1.5, negate: true) == -3.0 { return 1; } return 0; };"),
        1
    );
    let errors = engine_with_native(c_fastmath())
        .check_source(r#"function main() -> int { return fastMath::mulD(a: "x", b: 1); };"#)
        .expect_err("type error expected");
    assert!(errors
        .iter()
        .map(|e| e.to_string())
        .collect::<String>()
        .contains("expects int"));
}

#[test]
fn strings_are_borrowed_and_created() {
    assert_eq!(
        run_int(r#"function main() -> int { return fastMath::strLen(text: "héllo"); };"#),
        6
    );
    assert_eq!(
        run_int(
            r#"function main() -> int { if fastMath::shout(text: "abc") == "ABC!" { return 7; } return 0; };"#
        ),
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
    assert_eq!(
        run_int("function main() -> int { return len(value: fastMath::iota(n: 5)); };"),
        5
    );
    assert_eq!(run_int("function main() -> int { var p = fastMath::pair(a: 3, b: 4); return p.sum.asInt() * 100 + p.product.asInt(); };"), 712);
}

#[test]
fn native_calls_back_into_spar() {
    let src = r#"
        function main() -> int {
            var factor: int = 10;
            var out: [int] = fastMath::mapInts(values: [1, 2, 3], f: |value: int| value * factor + 1);
            return out[0] + out[1] + out[2];
        };
    "#;
    assert_eq!(run_int(src), 11 + 21 + 31);
    // a named function works too, and errors raised inside the callback propagate with their text
    let err = run_err(
        r#"
        function boom(value: int) -> int { assert(condition: value < 2, message: "callback rejected"); return value; };
        function main() -> int { fastMath::mapInts(values: [1, 2, 3], f: boom); return 0; };
    "#,
    );
    assert!(err.contains("callback rejected"), "{err}");
}

#[test]
fn native_errors_carry_message_and_span() {
    let msg = run_err("function main() -> int { return fastMath::fail(); };");
    assert!(msg.contains("deliberate failure from C"), "{msg}");
    let msg =
        run_err("function main() -> int { return fastMath::add(a: 9223372036854775807, b: 1); };");
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
    let err = native_module::load_module(Path::new("/nonexistent/lib.so"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot load native module"), "{err}");
    // libm exists everywhere but does not export the Spar entry symbol.
    let libm = [
        "/usr/lib/libm.so.6",
        "/lib/x86_64-linux-gnu/libm.so.6",
        "/usr/lib64/libm.so.6",
    ]
    .iter()
    .map(Path::new)
    .find(|p| p.exists());
    if let Some(libm) = libm {
        let err = native_module::load_module(libm).unwrap_err().to_string();
        assert!(err.contains("does not export"), "{err}");
    }
}

// ---------------------------------------------------------------------------------------------
// Rust SDK module (spar-native + macros), built with cargo.
// ---------------------------------------------------------------------------------------------

fn rust_fastarray() -> &'static Path {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let dir = sys_dir().join("examples/native/rust-fastarray");
        let out = Command::new("cargo")
            .args(["build", "--release", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .output()
            .expect("cargo");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir.join("target/release/librust_fastarray.so")
    })
}

fn run_rust(source: &str) -> Result<i32, String> {
    let mut natives = CompileOptions::default().natives;
    native_module::load_into_registry(rust_fastarray(), &mut natives).expect("load rust module");
    Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .execute_source(source)
    .map(|o| o.exit_status)
    .map_err(|e| {
        e.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[test]
fn rust_module_zero_copy_buffer_round_trip() {
    let src = r#"
        function main() -> int {
            var b: Buffer = fastArray::linspace(n: 5);
            var total: float = fastArray::sum(values: b);
            if total != 2.5 { return 1; }
            fastArray::scale(buf: b, k: 2.0);
            if fastArray::sum(values: b) != 5.0 { return 2; }
            if fastArray::dot(a: b, b: b) != 2.0 * 2.0 * (0.0 + 0.0625 + 0.25 + 0.5625 + 1.0) { return 3; }
            return 0;
        };
    "#;
    assert_eq!(run_rust(src), Ok(0));
}

#[test]
fn rust_module_lists_strings_and_threads() {
    let src = r#"
        function main() -> int {
            if fastArray::sum(values: [1.0, 2.0, 3.5]) != 6.5 { return 1; }
            if fastArray::wordCount(text: "a bb  ccc") != 3 { return 2; }
            if fastArray::reverse(text: "abc") != "cba" { return 3; }
            var b: Buffer = fastArray::linspace(n: 1000001);
            if fastArray::parSum(values: b, threads: 4) < 499999.0 { return 4; }
            return 0;
        };
    "#;
    assert_eq!(run_rust(src), Ok(0));
}

#[test]
fn rust_errors_and_panics_are_contained() {
    let err =
        run_rust("function main() -> int { fastArray::dot(a: [1.0], b: [1.0, 2.0]); return 0; };")
            .unwrap_err();
    assert!(err.contains("length mismatch"), "{err}");
    let err = run_rust("function main() -> int { return fastArray::explode(); };").unwrap_err();
    assert!(err.contains("boom from rust"), "{err}");
    // The runtime is still usable after a contained panic.
    assert_eq!(
        run_rust("function main() -> int { return fastArray::wordCount(text: \"x y\"); };"),
        Ok(2)
    );
}

#[test]
fn mutable_borrow_of_list_is_rejected() {
    let err = run_rust("function main() -> int { var xs: [float] = [1.0, 2.0]; fastArray::scale(buf: xs, k: 2.0); return 0; };");
    assert!(err.is_err());
}

// ---------------------------------------------------------------------------------------------
// C++ module (spar_native.hpp): strings, RAII borrows, exception conversion.
// ---------------------------------------------------------------------------------------------

fn cpp_textkit() -> &'static Path {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let dir = sys_dir().join("examples/native/cpp-textkit");
        let out = Command::new(dir.join("build.sh")).output().expect("c++");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir.join("libtextkit.so")
    })
}

fn run_cpp(source: &str) -> Result<i32, String> {
    let mut natives = CompileOptions::default().natives;
    native_module::load_into_registry(cpp_textkit(), &mut natives).expect("load cpp module");
    Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .execute_source(source)
    .map(|o| o.exit_status)
    .map_err(|e| {
        e.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[test]
fn cpp_module_strings_buffers_and_exceptions() {
    assert_eq!(
        run_cpp(
            r#"function main() -> int { if textKit::upper(text: "abc") != "ABC" { return 1; } if textKit::mean(values: [1.0, 2.0, 6.0]) != 3.0 { return 2; } return 0; };"#
        ),
        Ok(0)
    );
    let err = run_cpp("function main() -> int { return textKit::throws(); };").unwrap_err();
    assert!(err.contains("C++ exception"), "{err}");
    let err = run_cpp(
        "function main() -> int { var e: [float] = []; textKit::mean(values: e); return 0; };",
    )
    .unwrap_err();
    assert!(err.contains("mean of empty input"), "{err}");
}

#[test]
fn cpp_module_zero_copy_bytes_from_a_file() {
    let file = std::env::temp_dir().join(format!("spar-bytes-{}.bin", std::process::id()));
    std::fs::write(&file, [1u8, 2, 3, 250]).unwrap();
    let src = format!(
        r#"import pkg {{ readBytes }} from "std/fs";
        function main() -> int {{ var b: Bytes = readBytes(path: "{}"); return textKit::checksum(data: b); }};"#,
        file.display()
    );
    // FNV-1a over [1,2,3,250]
    let mut h: u32 = 2166136261;
    for c in [1u8, 2, 3, 250] {
        h = (h ^ c as u32).wrapping_mul(16777619);
    }
    let expected = (h & 0x7fff) as i32;
    assert_eq!(run_cpp(&src), Ok(expected));
}

// ---------------------------------------------------------------------------------------------
// Async: completion from a native worker thread through the scheduler's promise table.
// ---------------------------------------------------------------------------------------------

static ASYNC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn async_native_completes_from_a_worker_thread() {
    let _g = ASYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let src = r#"
        async function main() -> int {
            var p = fastMath::delayedAdd(a: 20, b: 22, millis: 30);
            var v: int = await p;
            return v;
        };
    "#;
    assert_eq!(run_int(src), 42);
}

#[test]
fn async_native_operations_overlap() {
    let _g = ASYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    c_fastmath(); // build outside the timed region
    let src = r#"
        async function main() -> int {
            var a = fastMath::delayedAdd(a: 1, b: 1, millis: 300);
            var b = fastMath::delayedAdd(a: 2, b: 2, millis: 300);
            var x: int = await a;
            var y: int = await b;
            return x + y;
        };
    "#;
    let t = std::time::Instant::now();
    assert_eq!(run_int(src), 6);
    assert!(
        t.elapsed() < std::time::Duration::from_millis(550),
        "operations ran sequentially: {:?}",
        t.elapsed()
    );
}

#[test]
fn async_failure_and_abandonment_surface_as_errors_not_hangs() {
    let _g = ASYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let err = run_err("async function main() -> int { var p = fastMath::delayedFail(millis: 10); var v: int = await p; return v; };");
    assert!(err.contains("async failure from C worker"), "{err}");
    let err = run_err("async function main() -> int { var p = fastMath::abandoned(); var v: int = await p; return v; };");
    assert!(err.contains("released without completing"), "{err}");
}

#[test]
fn async_double_completion_is_rejected() {
    let _g = ASYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(run_int("async function main() -> int { var v: int = await fastMath::delayedAdd(a: 1, b: 0, millis: 10); return v; };"), 1);
    std::thread::sleep(std::time::Duration::from_millis(50));
    // SPAR_E_INVALID_STATE == 13 was recorded by the worker on its second async_complete.
    assert_eq!(
        run_int("function main() -> int { return fastMath::secondStatus(); };"),
        13
    );
    assert_eq!(native_module::live_async_ops(), 0, "async operation leaked");
}

// ---------------------------------------------------------------------------------------------
// ABI compatibility: a module compiled against the frozen 0.1 header keeps loading on the newer
// runtime, and incompatible descriptors are rejected with a diagnostic before `init` runs.
// ---------------------------------------------------------------------------------------------

fn build_fixture(tag: &str, defines: &[&str]) -> PathBuf {
    let dir = sys_dir().join("tests/fixtures/abi-0.1");
    let out_dir = std::env::temp_dir().join(format!("spar-fixture-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let lib = out_dir.join("libfixture.so");
    let out = Command::new("cc")
        .args(["-std=c11", "-fPIC", "-fvisibility=hidden", "-shared"])
        .args(defines)
        .arg(format!("-I{}", dir.display()))
        .arg(dir.join("fixture.c"))
        .arg("-o")
        .arg(&lib)
        .output()
        .expect("cc");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    lib
}

#[test]
fn module_built_against_old_header_still_loads_and_runs() {
    let lib = build_fixture("ok", &[]);
    assert!(SPAR_NATIVE_MINOR_NEWER());
    let mut natives = CompileOptions::default().natives;
    native_module::load_into_registry(&lib, &mut natives)
        .expect("old module loads on newer runtime");
    let out = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
    .execute_source("function main() -> int { return oldMod::twice(x: 21); };")
    .unwrap();
    assert_eq!(out.exit_status, 42);
}

#[allow(non_snake_case)]
fn SPAR_NATIVE_MINOR_NEWER() -> bool {
    spar_native_sys::SPAR_NATIVE_ABI_MINOR > 1
}

#[test]
fn incompatible_modules_are_rejected_with_clear_diagnostics() {
    let cases: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "major",
            vec!["-DFX_MAJOR=7", "-DFX_NAME=\"badMajor\""],
            "ABI major 7",
        ),
        (
            "minor",
            vec!["-DFX_MIN_MINOR=99", "-DFX_NAME=\"badMinor\""],
            "needs ABI 0.99",
        ),
        (
            "caps",
            vec!["-DFX_CAPS=(1ull<<40)", "-DFX_NAME=\"badCaps\""],
            "capabilities",
        ),
        (
            "target",
            vec![
                "-DFX_TARGET=\"riscv64-unknown-plan9\"",
                "-DFX_NAME=\"badTarget\"",
            ],
            "built for target",
        ),
        ("name", vec!["-DFX_NAME=\"1bad\""], "not a valid identifier"),
    ];
    for (tag, defs, expect) in cases {
        let lib = build_fixture(tag, &defs);
        let err = native_module::load_module(&lib).unwrap_err().to_string();
        assert!(err.contains(expect), "{tag}: {err}");
    }
}

#[test]
fn declared_native_types_are_distinct_and_typechecked() {
    let dir = sys_dir().join("examples/native/bench-kernels");
    assert!(Command::new(dir.join("build.sh"))
        .status()
        .unwrap()
        .success());
    let mut natives = CompileOptions::default().natives;
    native_module::load_into_registry(&dir.join("libbenchkit.so"), &mut natives).unwrap();
    let engine = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    });
    let ok = engine
        .execute_source("function main() -> int { var c: Counter = benchkit::counterNew(); benchkit::counterBump(counter: c); return benchkit::counterBump(counter: c); };")
        .unwrap();
    assert_eq!(ok.exit_status, 2);
    // a Buffer is not a Counter
    let errors = engine
        .check_source("function main() -> int { var b: Buffer = benchkit::makeBuf(n: 4); return benchkit::counterBump(counter: b); };")
        .expect_err("Buffer must not be accepted as Counter");
    assert!(errors
        .iter()
        .map(|e| e.to_string())
        .collect::<String>()
        .contains("Counter"));
}

#[test]
fn rust_sdk_resources_are_typed_and_finalized() {
    let src = r#"
        function main() -> int {
            var t: Tally = fastArray::tallyNew(start: 10);
            fastArray::tallyAdd(tally: t, n: 5);
            return fastArray::tallyAdd(tally: t, n: 7);
        };
    "#;
    assert_eq!(run_rust(src), Ok(22));
    // resource of the wrong Spar type is rejected at compile time
    let err = run_rust("function main() -> int { var b: Buffer = fastArray::linspace(n: 3); return fastArray::tallyAdd(tally: b, n: 1); };").unwrap_err();
    assert!(err.contains("Tally"), "{err}");
}
