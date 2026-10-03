//! Native extension packages: manifest `struct Native`, target selection, integrity, confinement,
//! and running a project through the real CLI.
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/native_build.rs"]
mod native_build;

use sha2::{Digest, Sha256};
use spar::package::{NativeSpec, PackageManifest};

fn build_fastmath(out_dir: &Path) -> PathBuf {
    native_build::build_fastmath(out_dir, false)
}

fn fastmath_artifact() -> String {
    format!(
        "native/{}fastmath{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
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
        Some("real") => format!("    {key}Sha256: str = \"{real_sha}\";\n"),
        Some(other) => format!("    {key}Sha256: str = \"{other}\";\n"),
        None => String::new(),
    };
    std::fs::write(
        dir.path().join("spar.package.spar"),
        format!(
            "struct Package {{\n    name: str = \"app\";\n    version: str = \"0.1.0\";\n    kind: str = \"application\";\n    entry: str = \"src/main.spar\";\n}};\n\nstruct Native {{\n    module: str = \"fastMath\";\n    abi: str = \"spar-native-1\";\n    capabilities: str = \"strings,bytes,lists,records,callbacks,async\";\n    {key}: str = \"{artifact_rel}\";\n{sha_line}}};\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main.spar"),
        "import fastMath;\nfunction main() -> int {\n    println(value: \"answer = ${fastMath.add(a: 40, b: 2)}\");\n    return 0;\n};\n",
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
    let src = "struct Package {\n    name: str = \"p\";\n    version: str = \"1.0.0\";\n    kind: str = \"library\";\n};\nstruct Native {\n    module: str = \"m\";\n    abi: str = \"spar-native-1\";\n    linuxX8664Gnu: str = \"n/a.so\";\n    linuxX8664GnuSha256: str = \"0000000000000000000000000000000000000000000000000000000000000000\";\n};\n";
    let m = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap();
    let spec: &NativeSpec = m.native.as_ref().unwrap();
    assert_eq!(spec.module, "m");
    assert!(spec.artifacts["linuxX8664Gnu"].1.is_some());
    let again = PackageManifest::parse(&m.render(), Path::new("spar.package.spar")).unwrap();
    assert_eq!(again.native, m.native);
    // wrong abi and stray fields are rejected
    let bad = src.replace("spar-native-1", "spar-native-9");
    assert!(PackageManifest::parse(&bad, Path::new("x")).is_err());
    let bad = src.replace(
        "    module: str = \"m\";\n",
        "    module: str = \"m\";\n    nonsense: str = \"x\";\n",
    );
    assert!(PackageManifest::parse(&bad, Path::new("x")).is_err());
}

#[test]
fn legacy_native_target_keys_render_in_camel_case() {
    let source = "struct Package { name: str = \"p\"; version: str = \"1.0.0\"; kind: str = \"library\"; };\nstruct Native { module: str = \"m\"; abi: str = \"spar-native-1\"; linux_x86_64_gnu: str = \"n/a.so\"; };\n";
    let manifest = PackageManifest::parse(source, Path::new("spar.package.spar")).unwrap();
    assert!(manifest.render().contains("linuxX8664Gnu: str"));
    assert!(!manifest.render().contains("linux_x86_64_gnu: str"));
}

#[test]
fn native_interface_mismatch_is_rejected_at_runtime() {
    let dir = project(None, &fastmath_artifact());
    let path = dir.path().join("spar.package.spar");
    let manifest = std::fs::read_to_string(&path).unwrap().replace(
        "    capabilities: str",
        "    interface: str = \"native/interface.json\";\n    capabilities: str",
    );
    std::fs::write(path, manifest).unwrap();
    std::fs::write(
        dir.path().join("native/interface.json"),
        r#"{"format_version":1,"module":"fastMath","types":[],"functions":[]}"#,
    )
    .unwrap();
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("native interface for 'fastMath' has functions that differ"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn project_with_native_package_runs_through_the_cli() {
    let dir = project(Some("real"), &fastmath_artifact());
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("answer = 42"));
}

#[test]
fn manifest_abi_must_match_the_loaded_artifact() {
    let dir = project(None, &fastmath_artifact());
    let path = dir.path().join("spar.package.spar");
    let manifest = std::fs::read_to_string(&path)
        .unwrap()
        .replace("spar-native-1", "spar-native-0");
    std::fs::write(&path, manifest).unwrap();
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("manifest says 'spar-native-0'"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn tampered_artifact_is_rejected_before_loading() {
    let dir = project(Some(&"0".repeat(64)), &fastmath_artifact());
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
    let dir = project(None, &fastmath_artifact());
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
    let dir = project(None, &fastmath_artifact());
    let manifest = std::fs::read_to_string(dir.path().join("spar.package.spar"))
        .unwrap()
        .replace(
            &target_key(),
            if target_key() == "linuxX8664Gnu" {
                "macosAarch64"
            } else {
                "linuxX8664Gnu"
            },
        );
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
    let dir = project(Some("real"), &fastmath_artifact());
    assert!(spar(dir.path(), &["exec", "src/main.spar"], &[])
        .status
        .success());
    set_runtime(dir.path(), "    native: bool = false;\n");
    let out = spar(dir.path(), &["exec", "src/main.spar"], &[]);
    assert!(disabled(&out), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn env_var_beats_manifest() {
    let dir = project(Some("real"), &fastmath_artifact());
    set_runtime(dir.path(), "    native: bool = true;\n");
    let out = spar(
        dir.path(),
        &["exec", "src/main.spar"],
        &[("SPAR_NO_NATIVE", "1")],
    );
    assert!(disabled(&out), "env must override manifest");
}

#[test]
fn cli_flag_beats_env_var_and_manifest() {
    let dir = project(Some("real"), &fastmath_artifact());
    set_runtime(dir.path(), "    native: bool = false;\n");
    let out = spar(
        dir.path(),
        &["exec", "--runtime", "native=true", "src/main.spar"],
        &[("SPAR_NO_NATIVE", "1")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let dir = project(Some("real"), &fastmath_artifact());
    set_runtime(dir.path(), "    native: bool = false;\n");
    let out = spar(dir.path(), &["run", "--app", "app"], &[]);
    assert!(disabled(&out), "{}", String::from_utf8_lossy(&out.stderr));
    let out = spar(
        dir.path(),
        &["run", "--runtime", "native=true", "--app", "app"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
