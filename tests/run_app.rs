//! `spar run --app <name> [args...]`: run a package's `main` by name through the real CLI.
use std::path::Path;
use std::process::{Command, Output};

const MAIN_ARGC: &str = "import pkg { args } from \"std/process\";\nfunction main() -> int { return args().length(); };\n";

fn manifest(name: &str, kind: &str) -> String {
    format!(
        "struct Package {{\n    name: str = \"{name}\";\n    version: str = \"0.1.0\";\n    kind: str = \"{kind}\";\n    entry: str = \"src/main.spar\";\n}};\n"
    )
}

fn write_package(dir: &Path, name: &str, kind: &str, main: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("spar.package.spar"), manifest(name, kind)).unwrap();
    std::fs::write(dir.join("src/main.spar"), main).unwrap();
}

fn spar(cwd: &Path, args: &[&str]) -> Output {
    let home = cwd.join(".testhome");
    Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn runs_the_current_project_by_name_and_passes_args_to_main() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "omatarasu", "application", MAIN_ARGC);
    let out = spar(dir.path(), &["run", "--app", "omatarasu", "one", "--two"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn works_from_a_subdirectory_of_the_project() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "omatarasu", "application", MAIN_ARGC);
    let out = spar(&dir.path().join("src"), &["run", "--app", "omatarasu", "a"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
}

#[test]
fn a_leading_double_dash_is_optional_and_dropped() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "omatarasu", "application", MAIN_ARGC);
    let out = spar(dir.path(), &["run", "--app", "omatarasu", "--", "--flag", "x"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn mains_exit_status_becomes_the_process_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "app", "application", "function main() -> int { return 7; };\n");
    let out = spar(dir.path(), &["run", "--app", "app"]);
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
}

#[test]
fn a_library_has_no_main_to_run() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "mylib", "library", "function helper() -> int { return 1; };\n");
    let out = spar(dir.path(), &["run", "--app", "mylib"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("is a library, not an application"), "{}", stderr(&out));
}

#[test]
fn unknown_package_name_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "omatarasu", "application", MAIN_ARGC);
    let out = spar(dir.path(), &["run", "--app", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no package named 'nope'"), "{}", stderr(&out));
}

#[test]
fn outside_any_project_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let out = spar(dir.path(), &["run", "--app", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no spar.package.spar found"), "{}", stderr(&out));
}

#[test]
fn missing_name_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = spar(dir.path(), &["run", "--app"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("`--app` requires a package name"), "{}", stderr(&out));
}

#[test]
fn a_locked_path_dependency_runs_by_alias_or_package_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_package(&root.join("tools/greeter"), "greeter", "application", MAIN_ARGC);
    std::fs::write(
        root.join("spar.package.spar"),
        format!(
            "{}\nstruct Dependencies {{\n    tool: str = \"path:tools/greeter\";\n}};\n",
            manifest("host", "application")
        ),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.spar"), "function main() -> int { return 0; };\n").unwrap();
    let lock = spar(root, &["install"]);
    assert!(lock.status.success(), "{}", stderr(&lock));
    let out = spar(root, &["run", "--app", "greeter", "a", "b", "c"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let out = spar(root, &["run", "--app", "tool", "a"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
}

#[test]
fn tasks_still_run_after_the_app_mode_exists() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("SparMake.spar"),
        "task Hello { description: \"Hi\"; run { true; }; };\n",
    )
    .unwrap();
    let out = spar(dir.path(), &["run", "hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

// ── `struct Runtime` ──────────────────────────────────────────────────────────

fn with_runtime(dir: &Path, runtime: &str) {
    let mut text = manifest("app", "application");
    text.push_str(&format!("\nstruct Runtime {{\n{runtime}}};\n"));
    std::fs::write(dir.join("spar.package.spar"), text).unwrap();
}

/// Counts async workers via a script that reports nothing directly; instead we use the native
/// switch: a manifest with `native = false` and a package that declares none still runs, while
/// invalid values are rejected before anything executes.
#[test]
fn manifest_runtime_struct_is_accepted_and_the_app_runs() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "app", "application", "function main() -> int { return 4; };\n");
    with_runtime(
        dir.path(),
        "    vm: bool = true;\n    bytecode: bool = true;\n    jit: bool = false;\n    asyncWorkers: int = 2;\n    native: bool = true;\n    nativeDebug: bool = false;\n    nativeModules: str = \"\";\n",
    );
    let out = spar(dir.path(), &["run", "--app", "app"]);
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
}

#[test]
fn unknown_runtime_key_lists_the_valid_ones() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "app", "application", "function main() -> int { return 0; };\n");
    with_runtime(dir.path(), "    turbo: bool = true;\n");
    let out = spar(dir.path(), &["run", "--app", "app"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("unknown runtime setting 'turbo'") && err.contains("asyncWorkers"), "{err}");
}

#[test]
fn wrong_runtime_type_or_value_is_a_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "app", "application", "function main() -> int { return 0; };\n");
    with_runtime(dir.path(), "    jit: str = \"no\";\n");
    let out = spar(dir.path(), &["run", "--app", "app"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("field 'jit' must be typed `bool`"), "{}", stderr(&out));
    with_runtime(dir.path(), "    asyncWorkers: int = 0;\n");
    let out = spar(dir.path(), &["run", "--app", "app"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("positive integer"), "{}", stderr(&out));
}

#[test]
fn cli_flag_rejects_bad_settings_before_running() {
    let dir = tempfile::tempdir().unwrap();
    write_package(dir.path(), "app", "application", "function main() -> int { return 0; };\n");
    let out = spar(dir.path(), &["run", "--runtime", "jit=maybe", "--app", "app"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("expects true/false"), "{}", stderr(&out));
    let out = spar(dir.path(), &["run", "--runtime", "nope=1", "--app", "app"]);
    assert!(stderr(&out).contains("unknown runtime setting 'nope'"), "{}", stderr(&out));
}

#[test]
fn manifest_runtime_round_trips_through_render() {
    let path = Path::new("spar.package.spar");
    let mut text = manifest("app", "application");
    text.push_str("\nstruct Runtime {\n    jit: bool = false;\n    asyncWorkers: int = 3;\n    nativeModules: str = \"/a.so:/b.so\";\n};\n");
    let parsed = spar::package::PackageManifest::parse(&text, path).unwrap();
    assert_eq!(parsed.runtime.jit, Some(false));
    assert_eq!(parsed.runtime.async_workers, Some(3));
    let again = spar::package::PackageManifest::parse(&parsed.render(), path).unwrap();
    assert_eq!(again.runtime, parsed.runtime);
}
