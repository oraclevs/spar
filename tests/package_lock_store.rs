use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use spar::package::{
    LockedPackage, LockedSource, Lockfile, PackageError, PackageStore, StorePaths,
};

fn sample_package(name: &str) -> LockedPackage {
    LockedPackage {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        source: LockedSource::Github {
            owner: "owner".into(),
            repo: name.into(),
            revision: "0123456789abcdef0123456789abcdef01234567".into(),
        },
        integrity: Some(format!("sha256:{}", "d".repeat(64))),
        entry: "src/lib.spar".into(),
        dependencies: BTreeMap::new(),
    }
}

fn lockfile_in_order(names: &[&str]) -> Lockfile {
    let mut lockfile = Lockfile::default();
    for name in names {
        lockfile
            .root
            .insert(name.to_string(), format!("pkg-{name}"));
        lockfile
            .packages
            .insert(format!("pkg-{name}"), sample_package(name));
    }
    lockfile
}

#[test]
fn lockfile_serialization_is_stable_regardless_of_insertion_order() {
    assert_eq!(
        lockfile_in_order(&["a", "b"]).to_spar().unwrap(),
        lockfile_in_order(&["b", "a"]).to_spar().unwrap()
    );
}

#[test]
fn store_paths_use_injected_xdg_roots() {
    let paths = StorePaths::new(PathBuf::from("/data"), PathBuf::from("/cache"));
    assert_eq!(paths.store(), std::path::Path::new("/data/spar/store"));
    assert_eq!(paths.git_cache(), std::path::Path::new("/cache/spar/git"));
}

fn temp_store(root: &std::path::Path) -> PackageStore {
    PackageStore::new(StorePaths::new(root.join("data"), root.join("cache")))
}

#[test]
fn same_identity_reuses_one_snapshot_and_two_revisions_coexist() {
    let root = tempfile::tempdir().unwrap();
    let store = temp_store(root.path());

    let package_one = tempfile::tempdir().unwrap();
    fs::write(
        package_one.path().join("lib.spar"),
        "export var x: int = 1;",
    )
    .unwrap();
    let package_two = tempfile::tempdir().unwrap();
    fs::write(
        package_two.path().join("lib.spar"),
        "export var x: int = 2;",
    )
    .unwrap();

    let first = store.materialize("abc", package_one.path()).unwrap();
    let repeated = store.materialize("abc", package_one.path()).unwrap();
    let second = store.materialize("def", package_two.path()).unwrap();

    assert_eq!(first, repeated);
    assert_ne!(first, second);
    assert!(store.is_materialized("abc"));
    assert!(store.is_materialized("def"));
}

#[test]
fn altered_snapshot_fails_sha256_integrity_check() {
    let root = tempfile::tempdir().unwrap();
    let store = temp_store(root.path());
    let package = tempfile::tempdir().unwrap();
    fs::create_dir_all(package.path().join("src")).unwrap();
    fs::write(
        package.path().join("src/lib.spar"),
        "export var x: int = 1;",
    )
    .unwrap();

    let snapshot = store.materialize("abc", package.path()).unwrap();
    fs::write(snapshot.join("src/lib.spar"), "changed").unwrap();
    assert!(matches!(
        store.verify(&snapshot),
        Err(PackageError::IntegrityMismatch { .. })
    ));
}

#[test]
fn lockfile_write_atomically_and_read_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spar.package.lock.spar");
    let lockfile = lockfile_in_order(&["http", "colors"]);
    lockfile.write_atomically(&path).unwrap();
    assert_eq!(Lockfile::read(&path).unwrap(), lockfile);
}
