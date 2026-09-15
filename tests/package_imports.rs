use std::collections::BTreeMap;
use std::fs;

use spar::package::{
    LockedPackage, LockedSource, Lockfile, ModuleLocator, PackageStore, StorePaths,
};
use spar::{CompileOptions, Compiler};

/// Builds a store containing one package ("http", exporting `port`) and
/// the `Lockfile`/`ModuleLocator` a root project would use to depend on
/// it by the bare alias "http".
fn http_dependency_fixture(
    store_root: &std::path::Path,
) -> (Lockfile, PackageStore, ModuleLocator) {
    let store = PackageStore::new(StorePaths::new(
        store_root.join("data"),
        store_root.join("cache"),
    ));

    let http_source = tempfile::tempdir().unwrap();
    fs::write(
        http_source.path().join("lib.spar"),
        "export var port: int = 8080;\nfunction get(path: str) -> str { return path; };\n",
    )
    .unwrap();
    store
        .materialize("github-owner-http-abc123", http_source.path())
        .unwrap();

    let mut lockfile = Lockfile::default();
    lockfile
        .root
        .insert("http".to_string(), "github-owner-http-abc123".to_string());
    lockfile.packages.insert(
        "github-owner-http-abc123".to_string(),
        LockedPackage {
            name: "http".into(),
            version: "1.4.0".into(),
            source: LockedSource::Github {
                owner: "owner".into(),
                repo: "http".into(),
                revision: "abc123abc123abc123abc123abc123abc123abcd".into(),
            },
            integrity: None,
            entry: "lib.spar".into(),
            dependencies: BTreeMap::new(),
        },
    );

    let locator = ModuleLocator::for_root(lockfile.clone(), store.clone());
    (lockfile, store, locator)
}

#[test]
fn filesystem_like_import_keeps_current_relative_resolution() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("shared.spar"),
        "export var value: int = 7;\n",
    )
    .unwrap();
    // A ModuleLocator is configured, but the import string still looks
    // like a filesystem path, so it must never consult the locator.
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import \"shared.spar\" as shared;\nexport var value: int = shared.value;\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["value"],
        spar::ConfigValue::Int(7)
    );
}

#[test]
fn bare_import_uses_current_packages_lock_edges() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());

    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import \"http\" as http;\nexport var port: int = http::port;\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["port"],
        spar::ConfigValue::Int(8080)
    );
}

#[test]
fn selective_bare_import_loads_public_package_entry() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());

    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import { get } from \"http\";\nexport var routed: str = get(path: \"/x\");\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["routed"],
        spar::ConfigValue::Str("/x".to_string())
    );
}

#[test]
fn bare_import_with_no_locator_fails_as_a_missing_file_not_a_panic() {
    let temp = tempfile::tempdir().unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile("import \"http\" as http;\nexport var port: int = http::port;\n");

    assert!(!compilation.errors.is_empty());
    assert!(compilation
        .errors
        .iter()
        .any(|e| e.to_string().contains("cannot find import file")));
}

#[test]
fn missing_store_snapshot_is_a_clear_diagnostic_not_a_panic() {
    let temp = tempfile::tempdir().unwrap();
    // Lockfile references a package the store was never told to
    // materialize (simulates "checked out the lockfile but never ran
    // `spar install`").
    let store = PackageStore::new(StorePaths::new(
        temp.path().join("data"),
        temp.path().join("cache"),
    ));
    let mut lockfile = Lockfile::default();
    lockfile
        .root
        .insert("http".to_string(), "github-owner-http-missing".to_string());
    lockfile.packages.insert(
        "github-owner-http-missing".to_string(),
        LockedPackage {
            name: "http".into(),
            version: "1.4.0".into(),
            source: LockedSource::Github {
                owner: "owner".into(),
                repo: "http".into(),
                revision: "0000000000000000000000000000000000000000".into(),
            },
            integrity: None,
            entry: "lib.spar".into(),
            dependencies: BTreeMap::new(),
        },
    );
    let locator = ModuleLocator::for_root(lockfile, store);

    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import \"http\" as http;\nexport var port: int = http::port;\n");

    assert!(!compilation.errors.is_empty());
    assert!(compilation
        .errors
        .iter()
        .any(|e| e.to_string().contains("cannot find import file")));
}
