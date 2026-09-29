//! Native extension packages: manifest `struct Native`, target selection, integrity, confinement,
//! and running a project through the real CLI.
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use spar::package::{NativeSpec, PackageManifest};

fn build_fastmath(out_dir: &Path) -> PathBuf {
    let sys = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spar-native-sys");
    let lib = out_dir.join("libfastmath.so");
    let out = Command::new("cc")
        .args(["-std=c11", "-O2", "-fPIC", "-fvisibility=hidden", "-shared"])
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
}

fn target_key() -> String {
    spar::package::native::host_target_keys()[0].clone()
}

fn project(sha: Option<&str>, artifact_rel: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("native")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let lib = build_fastmath(&dir.path().join("native"));
    let real_sha: String = Sha256::digest(std::fs::read(&lib).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let key = target_key();
    let sha_line = match sha {
        Some("real") => format!("    {key}_sha256: str = \"{real_sha}\";\n"),
        Some(other) => format!("    {key}_sha256: str = \"{other}\";\n"),
        None => String::new(),
    };
    std::fs::write(
        dir.path().join("spar.package.spar"),
        format!(
            "struct Package {{\n    name: str = \"app\";\n    version: str = \"0.1.0\";\n    kind: str = \"application\";\n    entry: str = \"src/main.spar\";\n}};\n\nstruct Native {{\n    module: str = \"fastMath\";\n    abi: str = \"spar-native-0\";\n    capabilities: str = \"strings,bytes,lists,records,callbacks,async\";\n    {key}: str = \"{artifact_rel}\";\n{sha_line}}};\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main.spar"),
        "function main() -> int {\n    println(value: \"answer = ${fastMath::add(a: 40, b: 2)}\");\n    return 0;\n};\n",
    )
    .unwrap();
    dir
}

fn spar(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spar"));
    cmd.current_dir(dir)
        .args(args)
        .env_remove("SPAR_NATIVE_MODULES");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

#[test]
fn manifest_round_trips_native_section() {
    let src = "struct Package {\n    name: str = \"p\";\n    version: str = \"1.0.0\";\n    kind: str = \"library\";\n};\nstruct Native {\n    module: str = \"m\";\n    abi: str = \"spar-native-0\";\n    linux_x86_64_gnu: str = \"n/a.so\";\n    linux_x86_64_gnu_sha256: str = \"0000000000000000000000000000000000000000000000000000000000000000\";\n};\n";
    let m = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap();
    let spec: &NativeSpec = m.native.as_ref().unwrap();
    assert_eq!(spec.module, "m");
    assert!(spec.artifacts["linux_x86_64_gnu"].1.is_some());
    let again = PackageManifest::parse(&m.render(), Path::new("spar.package.spar")).unwrap();
    assert_eq!(again.native, m.native);
    // wrong abi and stray fields are rejected
    let bad = src.replace("spar-native-0", "spar-native-9");
    assert!(PackageManifest::parse(&bad, Path::new("x")).is_err());
    let bad = src.replace(
        "    module: str = \"m\";\n",
        "    module: str = \"m\";\n    nonsense: str = \"x\";\n",
    );
    assert!(PackageManifest::parse(&bad, Path::new("x")).is_err());
}

#[test]
fn project_with_native_package_runs_through_the_cli() {
    let dir = project(Some("real"), "native/libfastmath.so");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("answer = 42"));
}

#[test]
fn tampered_artifact_is_rejected_before_loading() {
    let dir = project(Some(&"0".repeat(64)), "native/libfastmath.so");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("integrity check"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn artifact_path_cannot_escape_the_package() {
    let dir = project(None, "../outside.so");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("inside the package"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = project(None, "/etc/passwd");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(!out.status.success());
}

#[test]
fn native_extensions_can_be_disabled() {
    let dir = project(None, "native/libfastmath.so");
    let out = spar(
        dir.path(),
        &["exec", "src/main.spar"],
        &[("SPAR_NO_NATIVE", "1")],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("native extensions are disabled"));
}

#[test]
fn missing_target_artifact_is_a_clear_error() {
    let dir = project(None, "native/libfastmath.so");
    let manifest = std::fs::read_to_string(dir.path().join("spar.package.spar"))
        .unwrap()
        .replace(&target_key(), "plan9_mips");
    std::fs::write(dir.path().join("spar.package.spar"), manifest).unwrap();
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no artifact for this target"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── runtime settings precedence: CLI flag > env var > manifest > default ──────

fn set_runtime(dir: &Path, body: &str) {
    let path = dir.join("spar.package.spar");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!("\nstruct Runtime {{\n{body}}};\n"));
    std::fs::write(path, text).unwrap();
}

fn disabled(out: &std::process::Output) -> bool {
    String::from_utf8_lossy(&out.stderr).contains("native extensions are disabled")
}

#[test]
fn manifest_can_disable_native_and_default_is_enabled() {
    let dir = project(Some("real"), "native/libfastmath.so");
    assert!(spar(dir.path(), &["exec", "src/main.spar"], &[]).status.success());
    set_runtime(dir.path(), "    native: bool = false;\n");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(disabled(&out), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn env_var_beats_manifest() {
    let dir = project(Some("real"), "native/libfastmath.so");
    set_runtime(dir.path(), "    native: bool = true;\n");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[("SPAR_NO_NATIVE", "1")]);
    assert!(disabled(&out), "env must override manifest");
}

#[test]
fn cli_flag_beats_env_var_and_manifest() {
    let dir = project(Some("real"), "native/libfastmath.so");
    set_runtime(dir.path(), "    native: bool = false;\n");
    let out = spar(
        dir.path(),
        &["exec", "--runtime", "native=true", "src/main.spar"],
        &[("SPAR_NO_NATIVE", "1")],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("answer = 42"));
    let out = spar(
        dir.path(),
        &["exec", "--runtime", "native=false", "src/main.spar"],
        &[],
    );
    assert!(disabled(&out));
}

#[test]
fn run_app_applies_manifest_runtime_and_cli_flag() {
    let dir = project(Some("real"), "native/libfastmath.so");
    set_runtime(dir.path(), "    native: bool = false;\n");
    let out = spar(dir.path(), &["run", "--app", "app"], &[]);
    assert!(disabled(&out), "{}", String::from_utf8_lossy(&out.stderr));
    let out = spar(dir.path(), &["run", "--runtime", "native=true", "--app", "app"], &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
