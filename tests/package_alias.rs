use spar::package::{
    commands, GitCommandProvider, NetworkPolicy, PackageKind, PackageStore, StorePaths,
};

fn store(dir: &std::path::Path) -> PackageStore {
    PackageStore::new(StorePaths::new(dir.join("data"), dir.join("cache")))
}

#[test]
fn hyphenated_alias_is_rejected_before_anything_is_written() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    commands::init(&project, "app", PackageKind::Application).unwrap();
    let manifest_before = std::fs::read_to_string(project.join("spar.package.spar")).unwrap();

    let error = commands::add(
        &project,
        "my-tools",
        "path:../tools",
        &GitCommandProvider::default(),
        NetworkPolicy::Offline,
        &store(temp.path()),
    )
    .unwrap_err();

    let text = error.to_string();
    assert!(
        text.contains("'my-tools' is not a valid dependency alias"),
        "{text}"
    );
    assert!(
        text.contains("myTools"),
        "suggests a valid spelling: {text}"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("spar.package.spar")).unwrap(),
        manifest_before
    );
}

#[test]
fn identifier_aliases_pass_alias_validation() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    commands::init(&project, "app", PackageKind::Application).unwrap();
    // The alias is valid; the request then fails for an unrelated reason
    // (the local path does not exist), which proves validation let it through.
    let error = commands::add(
        &project,
        "myTools_2",
        "path:../missing",
        &GitCommandProvider::default(),
        NetworkPolicy::Offline,
        &store(temp.path()),
    )
    .unwrap_err();
    assert!(
        !error.to_string().contains("not a valid dependency alias"),
        "{error}"
    );
}
