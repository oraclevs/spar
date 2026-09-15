use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use spar::package::{LockedPackage, LockedSource, Lockfile, PackageKind, PackageManifest};
use spar::{CompileOptions, Compiler};

fn manifest() -> PackageManifest {
    PackageManifest {
        name: "my-app".into(),
        version: semver::Version::new(1, 0, 0),
        kind: PackageKind::Application,
        entry: PathBuf::from("src/main.spar"),
        dependencies: BTreeMap::new(),
        overrides: BTreeMap::new(),
    }
}

#[test]
fn manifest_renderer_binds_package_to_builtin_shape() {
    let source = manifest().render();
    assert!(source.starts_with("[Package] -> SparPackage {"));
    assert!(PackageManifest::parse(&source, Path::new("spar.package.spar")).is_ok());
}

#[test]
fn compiler_preloads_manifest_shape_and_reports_missing_required_fields() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("spar.package.spar");
    let source = concat!(
        "[Package] -> SparPackage {\n",
        "    version: \"1.0.0\";\n",
        "    kind: \"application\";\n",
        "};\n",
    );
    let compilation = Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path(&path)
    })
    .compile(source);
    let messages = compilation
        .errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        messages.contains("missing required field 'name'"),
        "{messages}"
    );
    assert!(
        !messages.contains("undefined type 'SparPackage'"),
        "{messages}"
    );
}

#[test]
fn compiler_preloads_generated_lock_shapes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("spar.package.lock.spar");
    let source = concat!(
        "[Lock] -> SparPackageLock {\n",
        "    formatVersion: 1;\n",
        "    root: [SparLockedDependency] = [];\n",
        "    packages: [SparLockedPackage] = [];\n",
        "};\n",
    );
    let compilation = Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path(&path)
    })
    .compile(source);
    assert!(compilation.errors.is_empty(), "{:#?}", compilation.errors);
    let symbols = compilation.symbols.expect("metadata symbols");
    for name in [
        "SparPackageLock",
        "SparLockedPackage",
        "SparLockedDependency",
    ] {
        assert!(symbols.types.contains_key(name), "missing {name}");
    }
}

#[test]
fn compiler_checks_a_nonempty_generated_lock_against_builtin_shapes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("spar.package.lock.spar");
    let mut lock = Lockfile::default();
    lock.root.insert("toolkit".into(), "path-toolkit".into());
    lock.packages.insert(
        "path-toolkit".into(),
        LockedPackage {
            name: "toolkit".into(),
            version: "1.0.0".into(),
            source: LockedSource::Path {
                path: "/tmp/toolkit".into(),
            },
            integrity: None,
            entry: "src/lib.spar".into(),
            dependencies: BTreeMap::new(),
        },
    );
    let source = lock.to_spar().unwrap();
    let compilation = Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path(&path)
    })
    .compile(&source);
    assert!(compilation.errors.is_empty(), "{:#?}", compilation.errors);
}

#[test]
fn ordinary_spar_files_do_not_receive_reserved_metadata_types() {
    let compilation = Compiler::new(CompileOptions {
        evaluate: false,
        ..CompileOptions::for_path("app.spar")
    })
    .compile("[Config] { enabled: bool = true; };");
    assert!(compilation.errors.is_empty(), "{:#?}", compilation.errors);
    assert!(!compilation
        .symbols
        .expect("symbols")
        .types
        .contains_key("SparPackage"));
}
