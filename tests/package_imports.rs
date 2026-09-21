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
    fs::write(
        http_source.path().join("fs.spar"),
        "function readName() -> str { return \"package-submodule\"; };\n",
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
fn normal_import_does_not_fall_back_to_a_package_dependency() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());

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
        .any(|error| error.to_string().contains("cannot find import file 'http'")));
}

#[test]
fn normal_selective_import_does_not_fall_back_to_a_package_dependency() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());

    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import { get } from \"http\";\nexport var routed: str = get(path: \"/x\");\n");

    assert!(!compilation.errors.is_empty());
    assert!(compilation
        .errors
        .iter()
        .any(|error| error.to_string().contains("cannot find import file 'http'")));
}

#[test]
fn explicit_package_import_loads_dependency_entry() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());

    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import pkg { get } from \"http\";\nexport var routed: str = get(path: \"/pkg\");\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["routed"],
        spar::ConfigValue::Str("/pkg".to_string())
    );
}

#[test]
fn explicit_package_submodule_import_resolves_relative_to_entry_directory() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());

    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        locator: Some(locator),
        ..CompileOptions::default()
    })
    .compile("import pkg { readName } from \"http/fs\";\nexport var name: str = readName();\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["name"],
        spar::ConfigValue::Str("package-submodule".to_string())
    );
}

#[test]
fn extensionless_local_import_appends_spar_extension() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("shared.spar"),
        "export var value: int = 11;\n",
    )
    .unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile("import { value } from \"./shared\";\nexport var copied: int = value;\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["copied"],
        spar::ConfigValue::Int(11)
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
    .compile("import pkg \"http\" as http;\nexport var port: int = http::port;\n");

    assert!(!compilation.errors.is_empty());
    assert!(compilation
        .errors
        .iter()
        .any(|e| e.to_string().contains("cannot find import file")));
}

#[test]
fn package_imports_inside_dependencies_use_that_dependencies_lock_edges() {
    let temp = tempfile::tempdir().unwrap();
    let store = PackageStore::new(StorePaths::new(
        temp.path().join("data"),
        temp.path().join("cache"),
    ));

    let json_source = tempfile::tempdir().unwrap();
    fs::write(
        json_source.path().join("lib.spar"),
        "export var name: str = \"json-from-transitive-package\";\n",
    )
    .unwrap();
    store
        .materialize("github-owner-json-def456", json_source.path())
        .unwrap();

    let http_source = tempfile::tempdir().unwrap();
    fs::write(
        http_source.path().join("lib.spar"),
        concat!(
            "import pkg \"json\" as json;\n",
            "export var dependencyName: str = json::name;\n",
        ),
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
            dependencies: BTreeMap::from([(
                "json".to_string(),
                "github-owner-json-def456".to_string(),
            )]),
        },
    );
    lockfile.packages.insert(
        "github-owner-json-def456".to_string(),
        LockedPackage {
            name: "json".into(),
            version: "2.0.0".into(),
            source: LockedSource::Github {
                owner: "owner".into(),
                repo: "json".into(),
                revision: "def456def456def456def456def456def456defa".into(),
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
    .compile("import pkg \"http\" as http;\nexport var value: str = http::dependencyName;\n");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["value"],
        spar::ConfigValue::Str("json-from-transitive-package".to_string())
    );
}

#[test]
fn bundled_std_import_needs_no_manifest_or_lockfile() {
    let temp = tempfile::tempdir().unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile("import pkg { version } from \"std\"; export var v: str = version();");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["v"],
        spar::ConfigValue::Str("0.3.0".to_string())
    );
}

#[test]
fn bundled_std_submodule_resolves_without_lockfile() {
    let temp = tempfile::tempdir().unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile("import pkg { exists } from \"std/fs\"; export var present: bool = exists(path: \"./definitely-not-present\");");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert_eq!(
        compilation.result.unwrap().globals["present"],
        spar::ConfigValue::Bool(false)
    );
}

#[test]
fn selective_struct_import_carries_public_impl_methods_but_hides_private_helpers() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("user.spar"),
        r#"
        export struct User { name: str = "Obi"; };
        impl User {
            function displayName(self) -> str { return self.normalized(); };
            private function normalized(self) -> str { return self.name; };
        };
        "#,
    )
    .unwrap();

    let public_call = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(
        r#"
        import { User } from "./user";
        function main() -> str {
            var user = User();
            return user.displayName();
        };
        "#,
    );
    assert!(public_call.errors.is_empty(), "{:?}", public_call.errors);

    let private_call = Compiler::new(CompileOptions {
        base_dir: temp.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile(
        r#"
        import { User } from "./user";
        function main() -> str {
            var user = User();
            return user.normalized();
        };
        "#,
    );
    assert!(
        private_call.errors.iter().any(|error| {
            let message = error.to_string();
            message.contains("private") && message.contains("normalized")
        }),
        "{:?}",
        private_call.errors
    );
}

fn error_text(compilation: &spar::Compilation) -> String {
    compilation
        .errors
        .iter()
        .map(|error| format!("{error:?}"))
        .collect()
}

#[test]
fn engine_with_locator_resolves_package_imports() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let engine = spar::Engine::default()
        .with_base_dir(temp.path())
        .with_locator(locator);
    let compilation =
        engine.emit_source("import pkg { get } from \"http\";\nvar x: str = get(path: \"a\");\n");
    assert!(
        !error_text(&compilation).contains("cannot resolve package import"),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn unknown_package_alias_hint_names_the_configured_command() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let engine = spar::Engine::default()
        .with_base_dir(temp.path())
        .with_locator(locator)
        .with_package_command("pkg");
    let compilation = engine.emit_source("import pkg { x } from \"nope\";\n");
    let text = error_text(&compilation);
    assert!(text.contains("pkg add nope"), "{text}");
    assert!(!text.contains("spar install"), "{text}");
}

#[test]
fn default_package_command_is_spar() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let engine = spar::Engine::default()
        .with_base_dir(temp.path())
        .with_locator(locator);
    let compilation = engine.emit_source("import pkg { x } from \"nope\";\n");
    let text = error_text(&compilation);
    assert!(text.contains("spar add nope"), "{text}");
}
