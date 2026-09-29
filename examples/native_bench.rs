//! Native ABI benchmark suite. Run: `cargo run --release --example native_bench [filter]`.
//!
//! Method: Spar programs are timed at two loop sizes and the per-iteration cost is the slope, so
//! compile/startup cost cancels. Every result is the median of several runs. Bulk kernels are
//! compared against equivalent Rust and raw C-ABI code operating on the same data.
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use spar::{CompileOptions, Engine};

fn kit_path() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../spar-native-sys/examples/native/bench-kernels");
    let status = std::process::Command::new(dir.join("build.sh"))
        .status()
        .expect("build bench kernels");
    assert!(status.success());
    dir.join("libbenchkit.so")
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn time_fn(runs: usize, mut f: impl FnMut()) -> f64 {
    median(
        (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64()
            })
            .collect(),
    )
}

fn engine(kit: &PathBuf) -> Engine {
    let mut natives = CompileOptions::default().natives;
    spar::native_module::load_into_registry(kit, &mut natives).expect("load benchkit");
    Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    })
}

fn run_program(engine: &Engine, source: &str) -> f64 {
    let t = Instant::now();
    let out = engine
        .execute_source(source)
        .unwrap_or_else(|e| panic!("{e:?}\n{source}"));
    assert_eq!(out.exit_status, 0, "{source}");
    t.elapsed().as_secs_f64()
}

/// Per-iteration seconds: slope between two loop sizes, `{N}` substituted in `body_template`.
fn per_iter(engine: &Engine, setup: &str, body: &str, n1: u64, n2: u64) -> f64 {
    let (defs, init) = if setup.starts_with("fn ") {
        (setup, "")
    } else {
        ("", setup)
    };
    let make = |n: u64| {
        format!(
            "{defs}\nfn main() -> int {{\n {init}\n var mut acc: int = 0;\n var mut facc: float = 0.0;\n for i in range(end: {n}) {{\n {body}\n }}\n if acc == -1 {{ return 1; }}\n if facc < -1.0 {{ return 1; }}\n return 0;\n}};\n"
        )
    };
    let t1 = median((0..5).map(|_| run_program(engine, &make(n1))).collect());
    let t2 = median((0..5).map(|_| run_program(engine, &make(n2))).collect());
    ((t2 - t1) / (n2 - n1) as f64).max(0.0)
}

fn ns(secs: f64) -> String {
    format!("{:>10.2} ns", secs * 1e9)
}

fn gbps(bytes: f64, secs: f64) -> String {
    format!("{:>8.2} GB/s", bytes / secs / 1e9)
}

#[inline(never)]
fn rust_noop() -> i64 {
    black_box(0)
}
extern "C" fn rust_c_noop() -> i64 {
    black_box(0)
}
#[inline(never)]
fn rust_sum(v: &[f64]) -> f64 {
    v.iter().sum()
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default();
    let want = |name: &str| filter.is_empty() || name.contains(&filter);
    let kit = kit_path();
    println!(
        "# native ABI benchmarks (release build required), host {} {}",
        std::env::consts::ARCH,
        std::env::consts::OS
    );

    // ---- baselines without Spar ----
    let lib = unsafe { libloading::Library::new(&kit).unwrap() };
    let raw_noop: libloading::Symbol<unsafe extern "C" fn() -> i64> =
        unsafe { lib.get(b"bench_raw_noop").unwrap() };
    let raw_add: libloading::Symbol<unsafe extern "C" fn(i64, i64) -> i64> =
        unsafe { lib.get(b"bench_raw_add").unwrap() };
    let raw_sum: libloading::Symbol<unsafe extern "C" fn(*const f64, usize) -> f64> =
        unsafe { lib.get(b"bench_raw_sum_f64").unwrap() };
    let n = 200_000_000u64;
    if want("baseline") {
        let t = time_fn(5, || {
            let mut a = 0;
            for _ in 0..n {
                a += black_box(rust_noop());
            }
            black_box(a);
        });
        println!(
            "1  direct Rust no-op (inline(never))     {}",
            ns(t / n as f64)
        );
        let f: extern "C" fn() -> i64 = black_box(rust_c_noop);
        let t = time_fn(5, || {
            let mut a = 0;
            for _ in 0..n {
                a += black_box(f());
            }
            black_box(a);
        });
        println!(
            "2  extern \"C\" Rust no-op via fn pointer  {}",
            ns(t / n as f64)
        );
        let t = time_fn(5, || {
            let mut a = 0;
            for _ in 0..n {
                a += black_box(unsafe { raw_noop() });
            }
            black_box(a);
        });
        println!(
            "2b C-ABI no-op in shared library         {}",
            ns(t / n as f64)
        );
        let t = time_fn(5, || {
            let mut a = 0;
            for i in 0..n as i64 {
                a = unsafe { raw_add(black_box(a), black_box(i)) };
            }
            black_box(a);
        });
        println!(
            "4b C-ABI add(i64,i64) in shared library  {}",
            ns(t / n as f64)
        );
    }

    // ---- ABI cost only: registry-level calls, no interpreter around them ----
    if want("abi") || filter.is_empty() {
        let mut natives = CompileOptions::default().natives;
        spar::native_module::load_into_registry(&kit, &mut natives).unwrap();
        let mut ctx = spar::RuntimeContext::new(std::env::temp_dir());
        let span = spar::Span::dummy();
        println!("\nABI boundary only (NativeRegistry::call, no interpreter):");
        let calls: Vec<(&str, Vec<spar::Value>)> = vec![
            ("noop", vec![]),
            ("noopD", vec![]),
            ("add", vec![spar::Value::Int(1), spar::Value::Int(2)]),
            ("addD", vec![spar::Value::Int(1), spar::Value::Int(2)]),
            (
                "add4",
                vec![
                    spar::Value::Int(1),
                    spar::Value::Int(2),
                    spar::Value::Int(3),
                    spar::Value::Int(4),
                ],
            ),
            (
                "add4D",
                vec![
                    spar::Value::Int(1),
                    spar::Value::Int(2),
                    spar::Value::Int(3),
                    spar::Value::Int(4),
                ],
            ),
            (
                "addF",
                vec![spar::Value::Float(1.0), spar::Value::Float(2.0)],
            ),
            (
                "addFD",
                vec![spar::Value::Float(1.0), spar::Value::Float(2.0)],
            ),
            (
                "strLen",
                vec![spar::Value::String("hello native world".into())],
            ),
            (
                "strCopy",
                vec![spar::Value::String("hello native world".into())],
            ),
            (
                "fieldSum",
                vec![{
                    let mut r = spar::Record::new();
                    r.insert("x", spar::Value::Int(1));
                    r.insert("y", spar::Value::Int(2));
                    spar::Value::Object(r.into())
                }],
            ),
            ("sumBytes", vec![spar::Value::Bytes(vec![1; 16])]),
        ];
        let iters = 3_000_000u64;
        for (name, args) in calls {
            let (id, _) = natives
                .get("benchkit", name)
                .unwrap_or_else(|| panic!("{name}"));
            let t = time_fn(7, || {
                for _ in 0..iters {
                    black_box(natives.call(id, &mut ctx, black_box(&args), &span).unwrap());
                }
            });
            println!("  {name:<10} {}", ns(t / iters as f64));
        }
        // the same call resolved directly, as the floor for a Rust closure through the registry
        let mut floor_reg = spar::NativeRegistry::new();
        floor_reg
            .register(spar::NativeFunction::sync(
                "floor",
                "add",
                vec![
                    ("a", spar::ast::SparType::Int),
                    ("b", spar::ast::SparType::Int),
                ],
                spar::ast::SparType::Int,
                false,
                |_c, a| match (&a[0], &a[1]) {
                    (spar::Value::Int(x), spar::Value::Int(y)) => Ok(spar::Value::Int(x + y)),
                    _ => unreachable!(),
                },
            ))
            .unwrap();
        let (id, _) = floor_reg.get("floor", "add").unwrap();
        let args = vec![spar::Value::Int(1), spar::Value::Int(2)];
        let t = time_fn(7, || {
            for _ in 0..iters {
                black_box(
                    floor_reg
                        .call(id, &mut ctx, black_box(&args), &span)
                        .unwrap(),
                );
            }
        });
        println!(
            "  {:<10} {}  (built-in Rust closure through the same registry: the floor)",
            "rust-add",
            ns(t / iters as f64)
        );
    }

    // ---- Spar tiers ----
    let eng = engine(&kit);
    let (n1, n2) = (1_000_000, 5_000_000);
    let empty = per_iter(&eng, "", "acc = acc + 1;", n1, n2);
    let cases: Vec<(&str, &str, &str)> = vec![
        (
            "3  Spar function call (Spar-defined add)",
            "fn add2(a: int, b: int) -> int { return a + b; };",
            "acc = add2(a: acc, b: 1);",
        ),
        ("3  Spar->native no-op", "", "benchkit::noop();"),
        (
            "4  Spar->native add(int,int)",
            "",
            "acc = benchkit::add(a: acc, b: 1);",
        ),
        (
            "4d Spar->native addD (direct signature)",
            "",
            "acc = benchkit::addD(a: acc, b: 1);",
        ),
        (
            "5  Spar->native add4(int x4)",
            "",
            "acc = benchkit::add4(a: acc, b: 1, c: 0, d: 0);",
        ),
        (
            "5d Spar->native add4D (direct)",
            "",
            "acc = benchkit::add4D(a: acc, b: 1, c: 0, d: 0);",
        ),
        (
            "6  Spar->native addF(float,float)",
            "",
            "facc = benchkit::addF(x: facc, y: 1.0);",
        ),
        (
            "7  string length via borrowed view",
            "var s: str = \"hello native world\";",
            "acc = acc + benchkit::strLen(text: s);",
        ),
        (
            "8  string copy (view + string_new)",
            "var s: str = \"hello native world\";",
            "acc = acc + 1; var t: str = benchkit::strCopy(text: s);",
        ),
        (
            "16 native resource call",
            "var c: Counter = benchkit::counterNew();",
            "acc = benchkit::counterBump(counter: c);",
        ),
        (
            "15 record field access (2 fields)",
            "var r: Record = { x: 1; y: 2; };",
            "acc = acc + benchkit::fieldSum(rec: r);",
        ),
    ];
    println!("\nloop body baseline (acc = acc + 1): {}", ns(empty));
    for (name, setup, body) in cases {
        if !want(name) && !want("spar") {
            continue;
        }
        let t = per_iter(&eng, setup, body, n1, n2);
        println!(
            "{name:<44} {}  (net of loop: {})",
            ns(t),
            ns((t - empty).max(0.0))
        );
    }

    // ---- bulk ----
    println!();
    for &len in &[1_000_000usize, 10_000_000] {
        let v: Vec<f64> = (0..len).map(|i| i as f64).collect();
        let reps = if len == 1_000_000 { 200 } else { 20 };
        let t_rust = time_fn(7, || {
            for _ in 0..reps {
                black_box(rust_sum(black_box(&v)));
            }
        }) / reps as f64;
        let t_c = time_fn(7, || {
            for _ in 0..reps {
                black_box(unsafe { raw_sum(black_box(v.as_ptr()), v.len()) });
            }
        }) / reps as f64;
        let src = |body: &str, setup: &str| {
            format!("fn main() -> int {{ {setup} var mut facc: float = 0.0; for i in range(end: {reps}) {{ {body} }} if facc < -1.0 {{ return 1; }} return 0; }};")
        };
        let setup_buf = format!("var b: Buffer = benchkit::makeBuf(n: {len});");
        let base_setup = time_fn(5, || {
            run_program(&eng, &src("facc = facc + 1.0;", &setup_buf));
        });
        let t_buf = (time_fn(5, || {
            run_program(
                &eng,
                &src("facc = facc + benchkit::sumBuf(buf: b);", &setup_buf),
            );
        }) - base_setup)
            / reps as f64;
        let bytes = 8.0 * len as f64;
        println!("{} f64 elements ({:.0} MB)", len, bytes / 1e6);
        println!(
            "  direct Rust sum          {}  {}",
            ns(t_rust),
            gbps(bytes, t_rust)
        );
        println!(
            "  raw C-ABI sum            {}  {}",
            ns(t_c),
            gbps(bytes, t_c)
        );
        println!(
            "  Spar->native zero-copy   {}  {}  (overhead vs Rust: {:.1}%)",
            ns(t_buf),
            gbps(bytes, t_buf),
            (t_buf / t_rust - 1.0) * 100.0
        );
        if len == 1_000_000 {
            let setup_list = format!("var xs: [float] = benchkit::floatsList(n: {len});");
            let base = time_fn(3, || {
                run_program(&eng, &src("facc = facc + 1.0;", &setup_list));
            });
            let reps_l = 20;
            let src_l = format!("fn main() -> int {{ {setup_list} var mut facc: float = 0.0; for i in range(end: {reps_l}) {{ facc = facc + benchkit::sumList(values: xs); }} if facc < -1.0 {{ return 1; }} return 0; }};");
            let t_list = (time_fn(3, || {
                run_program(&eng, &src_l);
            }) - base)
                / reps_l as f64;
            println!(
                "  Spar->native list (copy) {}  {}",
                ns(t_list),
                gbps(bytes, t_list)
            );
        }
    }
    if want("extra") || filter.is_empty() {
        extra(&kit);
    }
    let _ = Duration::ZERO;
}

/// bytes / typed mutation / allocation / callbacks / async / threads
fn extra(kit: &PathBuf) {
    let lib = unsafe { libloading::Library::new(kit).unwrap() };
    let sys = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../spar-native-sys/examples/native");
    let fm = sys.join("c-fastmath");
    assert!(std::process::Command::new(fm.join("build.sh"))
        .status()
        .unwrap()
        .success());
    let ra = sys.join("rust-fastarray");
    assert!(std::process::Command::new("cargo")
        .args(["build", "--release", "--manifest-path"])
        .arg(ra.join("Cargo.toml"))
        .status()
        .unwrap()
        .success());
    let mut natives = CompileOptions::default().natives;
    spar::native_module::load_into_registry(kit, &mut natives).unwrap();
    spar::native_module::load_into_registry(&fm.join("libfastmath.so"), &mut natives).unwrap();
    spar::native_module::load_into_registry(
        &ra.join("target/release/librust_fastarray.so"),
        &mut natives,
    )
    .unwrap();
    let eng = Engine::new(CompileOptions {
        natives,
        ..CompileOptions::default()
    });
    println!("\nbulk bytes / mutation / allocation:");
    let len = 64_000_000usize;
    let reps = 10;
    let data = vec![1u8; len];
    let rust_bytes = time_fn(5, || {
        for _ in 0..reps {
            black_box(black_box(&data).iter().map(|b| *b as u64).sum::<u64>());
        }
    }) / reps as f64;
    let raw_u8: libloading::Symbol<unsafe extern "C" fn(*const u8, usize) -> u64> =
        unsafe { lib.get(b"bench_raw_sum_u8").unwrap() };
    let c_bytes = time_fn(5, || {
        for _ in 0..reps {
            black_box(unsafe { raw_u8(black_box(data.as_ptr()), data.len()) });
        }
    }) / reps as f64;
    let prog = |body: &str| {
        format!("fn main() -> int {{ var d: Bytes = benchkit::allocBytes(n: {len}); var mut acc: int = 0; for i in range(end: {reps}) {{ {body} }} if acc < 0 {{ return 1; }} return 0; }};")
    };
    let base = time_fn(3, || {
        run_program(&eng, &prog("acc = acc + 1;"));
    });
    let t = (time_fn(3, || {
        run_program(&eng, &prog("acc = acc + benchkit::sumBytes(data: d);"));
    }) - base)
        / reps as f64;
    println!(
        "  9  sumBytes {len} B zero-copy   {}  {}",
        ns(t),
        gbps(len as f64, t)
    );
    println!(
        "     raw C-ABI (same C kernel)   {}  {}",
        ns(c_bytes),
        gbps(len as f64, c_bytes)
    );
    println!(
        "     direct Rust (auto-vectorized, different kernel) {}  {}",
        ns(rust_bytes),
        gbps(len as f64, rust_bytes)
    );
    let n = 4_000_000usize;
    let mut v: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let k = black_box(1.0000001f64);
    let rust_scale = time_fn(5, || {
        for _ in 0..20 {
            for x in v.iter_mut() {
                *x *= k;
            }
            black_box(&mut v);
        }
    }) / 20.0;
    let prog = |body: &str| {
        format!("fn main() -> int {{ var b: Buffer = benchkit::makeBuf(n: {n}); var mut acc: int = 0; for i in range(end: 20) {{ {body} }} if acc < 0 {{ return 1; }} return 0; }};")
    };
    let base = time_fn(3, || {
        run_program(&eng, &prog("acc = acc + 1;"));
    });
    let t = (time_fn(3, || {
        run_program(&eng, &prog("benchkit::scaleBuf(buf: b, k: 1.0000001);"));
    }) - base)
        / 20.0;
    println!(
        "  11 scaleBuf {n} f64 in place    {}  {}   direct Rust {}  {}",
        ns(t),
        gbps(8.0 * n as f64, t),
        ns(rust_scale),
        gbps(8.0 * n as f64, rust_scale)
    );
    let base = time_fn(3, || {
        run_program(&eng, "fn main() -> int { var mut acc: int = 0; for i in range(end: 200) { acc = acc + 1; } return 0; };");
    });
    let t = (time_fn(3, || {
        run_program(&eng, "fn main() -> int { var mut acc: int = 0; for i in range(end: 200) { var d: Bytes = benchkit::allocBytes(n: 1000000); acc = acc + 1; } return 0; };");
    }) - base)
        / 200.0;
    println!("  14 native alloc + return 1 MB Bytes (copy)  {}", ns(t));

    println!("\ncallbacks (native loop + Spar callback per element, 100k elements):");
    let prog = |body: &str| {
        format!(
            "fn main() -> int {{ var xs: [int] = fastMath::iota(n: 100000); {body} return 0; }};"
        )
    };
    let base = time_fn(5, || {
        run_program(&eng, &prog(""));
    });
    let t_cb = time_fn(5, || {
        run_program(
            &eng,
            &prog("var ys: [int] = fastMath::mapInts(values: xs, f: |value: int| value + 1);"),
        );
    }) - base;
    let t_loop = time_fn(5, || {
        run_program(
            &eng,
            &prog("var mut total: int = 0; for x in xs { total = total + (x + 1); }"),
        );
    }) - base;
    let t_batch = time_fn(5, || {
        run_program(
            &eng,
            &prog("var f: float = fastMath::sumF64(values: [1.5, 2.5]);"),
        );
    }) - base;
    println!(
        "  17/18 native loop + Spar closure per element {}/element",
        ns(t_cb / 100_000.0)
    );
    println!(
        "        pure Spar for-loop doing the same      {}/element",
        ns(t_loop / 100_000.0)
    );
    println!(
        "        (a batch native op is one call: {})",
        ns(t_batch.max(0.0))
    );

    println!("\nasync round trip (delayedAdd, 0 ms, awaited one at a time):");
    let base = time_fn(3, || {
        run_program(&eng, "async function main() -> int { var mut acc: int = 0; for i in range(end: 2000) { acc = acc + 1; } return 0; };");
    });
    let t = (time_fn(3, || {
        run_program(&eng, "async function main() -> int { var mut acc: int = 0; for i in range(end: 2000) { var v: int = await fastMath::delayedAdd(a: 1, b: 1, millis: 0); acc = acc + v; } return 0; };");
    }) - base)
        / 2000.0;
    println!("  20 begin + worker thread + complete + await  {}", ns(t));

    println!("\nmulti-threaded native kernel (parSum, 10M f64, Rust module):");
    let prog = |threads: usize| {
        format!("fn main() -> int {{ var b: Buffer = fastArray::linspace(n: 10000000); var mut acc: float = 0.0; for i in range(end: 10) {{ acc = acc + fastArray::parSum(values: b, threads: {threads}); }} if acc < 0.0 {{ return 1; }} return 0; }};")
    };
    let base = time_fn(3, || {
        run_program(
            &eng,
            &prog(1).replace("fastArray::parSum(values: b, threads: 1)", "0.0"),
        );
    });
    let mut t1 = 0.0;
    for threads in [1usize, 2, 4, 8] {
        let t = (time_fn(3, || {
            run_program(&eng, &prog(threads));
        }) - base)
            / 10.0;
        if threads == 1 {
            t1 = t;
        }
        println!(
            "  {threads} threads  {}  {}  speedup {:.2}x",
            ns(t),
            gbps(80e6, t),
            t1 / t
        );
    }
}
