use std::path::{Path, PathBuf};
use std::process::Command;

/// Build the public-header C example with the host C toolchain.
pub fn build_fastmath(out_dir: &Path, cflags: bool) -> PathBuf {
    std::fs::create_dir_all(out_dir).expect("native build directory");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spar-native-sys");
    let source = root.join("examples/native/c-fastmath/fastmath.c");
    let output = out_dir.join(format!(
        "{}fastmath{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let compiler = std::env::var("CC").unwrap_or_else(|_| {
        if cfg!(target_env = "msvc") {
            "cl".into()
        } else {
            "cc".into()
        }
    });
    let mut command = Command::new(&compiler);
    command.current_dir(out_dir);
    if cfg!(target_env = "msvc") {
        command
            .args(["/nologo", "/LD", "/O2"])
            .arg(format!("/I{}", root.join("include").display()))
            .arg(&source)
            .arg("/link")
            .arg(format!("/OUT:{}", output.display()));
    } else {
        command.args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"]);
        if !cfg!(windows) {
            command.args(["-fPIC", "-fvisibility=hidden"]);
        }
        command.arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        });
        if cflags {
            command.args(
                std::env::var("SPAR_TEST_CFLAGS")
                    .unwrap_or_default()
                    .split_whitespace(),
            );
        }
        command
            .arg(format!("-I{}", root.join("include").display()))
            .arg(&source);
        if !cfg!(windows) {
            command.arg("-lm");
        }
        if cfg!(unix) && !cfg!(target_os = "macos") {
            command.arg("-lpthread");
        }
        command.arg("-o").arg(&output);
    }
    let result = command.output().expect("C compiler");
    assert!(
        result.status.success(),
        "C compiler {} failed:\n{}",
        compiler,
        String::from_utf8_lossy(&result.stderr)
    );
    output
}
