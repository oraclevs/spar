use std::path::{Path, PathBuf};
use std::process::Command;

pub fn build_fastmath(out_dir: &Path, cflags: bool) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spar-native-sys");
    build_shared(
        "fastmath",
        &root.join("examples/native/c-fastmath/fastmath.c"),
        &root.join("include"),
        out_dir,
        false,
        cflags,
        &[],
    )
}

/// Compile a native test module without writing generated binaries into a source checkout.
pub fn build_shared(
    name: &str,
    source: &Path,
    include: &Path,
    out_dir: &Path,
    cpp: bool,
    cflags: bool,
    defines: &[&str],
) -> PathBuf {
    std::fs::create_dir_all(out_dir).expect("native build directory");
    let output = out_dir.join(format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        name,
        std::env::consts::DLL_SUFFIX
    ));
    let compiler = std::env::var(if cpp { "CXX" } else { "CC" }).unwrap_or_else(|_| {
        if cfg!(target_env = "msvc") {
            "cl".into()
        } else if cpp {
            "c++".into()
        } else {
            "cc".into()
        }
    });
    let mut command = Command::new(&compiler);
    command.current_dir(out_dir);
    if cfg!(target_env = "msvc") {
        command.args(["/nologo", "/LD", "/O2"]);
        if cpp {
            command.args(["/EHsc", "/std:c++17"]);
        }
        for define in defines {
            command.arg(format!("/D{}", define.trim_start_matches("-D")));
        }
        command
            .arg(format!("/I{}", include.display()))
            .arg(source)
            .arg("/link")
            .arg(format!("/OUT:{}", output.display()));
    } else {
        command.args([
            if cpp { "-std=c++17" } else { "-std=c11" },
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
        ]);
        if !cfg!(windows) {
            command.args(["-fPIC", "-fvisibility=hidden"]);
        }
        command.arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        });
        command.args(defines);
        if cflags {
            command.args(
                std::env::var("SPAR_TEST_CFLAGS")
                    .unwrap_or_default()
                    .split_whitespace(),
            );
        }
        command.arg(format!("-I{}", include.display())).arg(source);
        if !cfg!(windows) {
            command.arg("-lm");
        }
        if cfg!(unix) && !cfg!(target_os = "macos") {
            command.arg("-lpthread");
        }
        command.arg("-o").arg(&output);
    }
    let result = command.output().expect("native C/C++ compiler");
    assert!(
        result.status.success(),
        "native compiler {} failed:\n{}",
        compiler,
        String::from_utf8_lossy(&result.stderr)
    );
    output
}
