use std::fs;
use std::path::Path;
use std::process::Command;

use spar::package::{
    commands, GitCommandProvider, GitHubSelector, LockedSource, NetworkPolicy, PackageError,
    PackageKind, PackageProvider, PackageStore, StorePaths,
};

fn git(args: &[&str], dir: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git must be installed to run this test");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(&["init", "--quiet", "-b", "main"], dir);
    git(&["config", "user.email", "test@example.com"], dir);
    git(&["config", "user.name", "Test"], dir);
}

fn commit_all(dir: &Path, message: &str) {
    git(&["add", "-A"], dir);
    git(&["commit", "--quiet", "-m", message], dir);
}

fn tag(dir: &Path, name: &str) {
    git(&["tag", name], dir);
}

fn write_colors_manifest(dir: &Path, version: &str, red_value: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("spar.package.spar"),
        format!(
            "[Package] {{\n    name: str = \"colors\";\n    version: str = \"{version}\";\n    kind: str = \"library\";\n}};\n",
        ),
    )
    .unwrap();
    fs::write(
        dir.join("src/lib.spar"),
        format!("export var red: str = \"{red_value}\";\n"),
    )
    .unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    project_dir: std::path::PathBuf,
    store: PackageStore,
    provider: GitCommandProvider,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let project_dir = root.path().join("project");
    fs::create_dir_all(&project_dir).unwrap();
    let store = PackageStore::new(StorePaths::new(
        root.path().join("data"),
        root.path().join("cache"),
    ));
    let remote_base = root.path().join("remote").join("owner");
    fs::create_dir_all(&remote_base).unwrap();
    let colors = remote_base.join("colors");
    init_repo(&colors);
    write_colors_manifest(&colors, "1.0.0", "#ff0000");
    commit_all(&colors, "v1.0.0");
    tag(&colors, "v1.0.0");

    let provider = GitCommandProvider::with_remote_base(
        root.path().join("remote").to_string_lossy().into_owned(),
    );

    Fixture {
        _root: root,
        project_dir,
        store,
        provider,
    }
}

struct PanicOnFetchProvider;
impl PackageProvider for PanicOnFetchProvider {
    fn fetch_github(
        &self,
        _owner: &str,
        _repo: &str,
        _selector: &GitHubSelector,
        _network: NetworkPolicy,
    ) -> Result<spar::package::FetchedRevision, PackageError> {
        panic!("fetch_github must not be called");
    }
}

#[test]
fn init_creates_manifest_and_entry_stub_and_refuses_to_overwrite() {
    let fx = fixture();
    let manifest = commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    assert_eq!(manifest.name, "myapp");
    assert!(fx.project_dir.join("spar.package.spar").is_file());
    assert!(fx.project_dir.join("src/main.spar").is_file());

    let error = commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap_err();
    assert!(matches!(error, PackageError::Conflict { .. }));
}

#[test]
fn add_resolves_materializes_and_writes_manifest_and_lock() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();

    let lockfile = commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();

    assert_eq!(lockfile.packages.len(), 1);
    let package = lockfile.packages.values().next().unwrap();
    assert_eq!(package.name, "colors");
    assert!(package.integrity.is_some());

    let manifest_text = fs::read_to_string(fx.project_dir.join("spar.package.spar")).unwrap();
    assert!(manifest_text.contains("github:owner/colors@1.0.0"));
    assert!(fx.project_dir.join("spar.lock").is_file());
}

#[test]
fn remove_drops_the_dependency_but_leaves_the_store_snapshot_alone() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    let lockfile = commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();
    let id = lockfile.root["colors"].clone();
    assert!(fx.store.is_materialized(&id));

    let lockfile = commands::remove(
        &fx.project_dir,
        "colors",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();
    assert!(lockfile.packages.is_empty());
    assert!(lockfile.root.is_empty());
    // The global store never deletes on `remove` — another project could
    // still be using this exact snapshot.
    assert!(fx.store.is_materialized(&id));

    let manifest_text = fs::read_to_string(fx.project_dir.join("spar.package.spar")).unwrap();
    assert!(!manifest_text.contains("colors"));
}

#[test]
fn install_from_an_existing_lock_never_calls_the_provider_when_already_materialized() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();

    // Everything is already in the store from `add` — offline install
    // with a provider that panics on any call must still succeed.
    let panic_provider = PanicOnFetchProvider;
    let lockfile = commands::install(
        &fx.project_dir,
        &panic_provider,
        NetworkPolicy::Offline,
        &fx.store,
    )
    .unwrap();
    assert_eq!(lockfile.packages.len(), 1);
}

#[test]
fn install_offline_with_a_missing_snapshot_fails_without_a_panic() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();

    // A fresh store with nothing materialized (simulates a fresh clone
    // of the project with the lockfile but no local store yet).
    let empty_store = PackageStore::new(StorePaths::new(
        fx.project_dir.join("empty-data"),
        fx.project_dir.join("empty-cache"),
    ));
    let error = commands::install(
        &fx.project_dir,
        &fx.provider,
        NetworkPolicy::Offline,
        &empty_store,
    )
    .unwrap_err();
    assert!(matches!(error, PackageError::Network { .. }));
}

#[test]
fn install_reuses_the_exact_locked_revision_even_if_the_tag_moved() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    let locked_before = commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();
    let LockedSource::Github {
        revision: locked_revision,
        ..
    } = locked_before
        .packages
        .values()
        .next()
        .unwrap()
        .source
        .clone()
    else {
        panic!("expected a github source");
    };

    // Force-move the "v1.0.0" tag to a new commit — simulates an
    // upstream retagging release after this project already locked it.
    let colors_repo = fx.project_dir.parent().unwrap().join("remote/owner/colors");
    write_colors_manifest(&colors_repo, "1.0.0", "#0000ff");
    commit_all(&colors_repo, "moved v1.0.0");
    git(&["tag", "-f", "v1.0.0"], &colors_repo);

    // A different, empty store, so `install` has to actually fetch —
    // proving it fetches the OLD locked commit, not the moved tag.
    let fresh_store = PackageStore::new(StorePaths::new(
        fx.project_dir.join("fresh-data"),
        fx.project_dir.join("fresh-cache"),
    ));
    let install_lockfile = commands::install(
        &fx.project_dir,
        &fx.provider,
        NetworkPolicy::Allow,
        &fresh_store,
    )
    .unwrap();
    let LockedSource::Github {
        revision: installed_revision,
        ..
    } = install_lockfile
        .packages
        .values()
        .next()
        .unwrap()
        .source
        .clone()
    else {
        panic!("expected a github source");
    };
    assert_eq!(locked_revision, installed_revision);
}

#[test]
fn update_can_move_the_lock_forward_to_a_newer_tag() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@^1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();
    let before = commands::install(
        &fx.project_dir,
        &fx.provider,
        NetworkPolicy::Offline,
        &fx.store,
    )
    .unwrap();
    let version_before = before.packages.values().next().unwrap().version.clone();
    assert_eq!(version_before, "1.0.0");

    // Publish a new compatible tag upstream.
    let colors_repo = fx.project_dir.parent().unwrap().join("remote/owner/colors");
    write_colors_manifest(&colors_repo, "1.1.0", "#00ff00");
    commit_all(&colors_repo, "v1.1.0");
    tag(&colors_repo, "v1.1.0");

    let after = commands::update(&fx.project_dir, None, &fx.provider, &fx.store).unwrap();
    let version_after = after.packages.values().next().unwrap().version.clone();
    assert_eq!(version_after, "1.1.0");
}

#[test]
fn tree_renders_the_lockfile_without_touching_the_network() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    commands::add(
        &fx.project_dir,
        "colors",
        "github:owner/colors@1.0.0",
        &fx.provider,
        NetworkPolicy::Allow,
        &fx.store,
    )
    .unwrap();

    let output = commands::tree(&fx.project_dir).unwrap();
    assert!(output.contains("colors"), "{output}");
    assert!(output.contains("colors@1.0.0"), "{output}");
}

#[test]
fn tree_on_a_project_with_no_lockfile_is_empty() {
    let fx = fixture();
    commands::init(&fx.project_dir, "myapp", PackageKind::Application).unwrap();
    assert_eq!(commands::tree(&fx.project_dir).unwrap(), "");
}
