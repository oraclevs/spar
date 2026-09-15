use std::fs;
use std::path::Path;
use std::process::Command;

use spar::package::{
    DependencyResolver, FetchedRevision, GitCommandProvider, GitHubSelector, NetworkPolicy,
    PackageError, PackageManifest, PackageProvider,
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

fn write_manifest(dir: &Path, name: &str, version: &str, deps: &[(&str, &str)]) {
    let mut deps_block = String::new();
    for (alias, request) in deps {
        deps_block.push_str(&format!("    {alias}: str = \"{request}\";\n"));
    }
    let deps_section = if deps.is_empty() {
        String::new()
    } else {
        format!("[Dependencies] {{\n{deps_block}}};\n")
    };
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("spar.package.spar"),
        format!(
            "[Package] {{\n    name: str = \"{name}\";\n    version: str = \"{version}\";\n    kind: str = \"library\";\n}};\n{deps_section}",
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/lib.spar"), "export var loaded: bool = true;").unwrap();
}

/// A `PackageProvider` that panics if it's ever called — used to prove a
/// resolution path never touches the network (an override satisfied
/// locally, or `NetworkPolicy::Offline` rejecting before any git call).
struct PanicOnFetchProvider;
impl PackageProvider for PanicOnFetchProvider {
    fn fetch_github(
        &self,
        _owner: &str,
        _repo: &str,
        _selector: &GitHubSelector,
        _network: NetworkPolicy,
    ) -> Result<FetchedRevision, PackageError> {
        panic!("fetch_github must not be called for this resolution");
    }
}

#[test]
fn transitive_graph_supports_two_versions_of_same_package() {
    let root = tempfile::tempdir().unwrap();
    let owner_dir = root.path().join("remote").join("owner");
    fs::create_dir_all(&owner_dir).unwrap();

    // "colors" repo with two tagged versions.
    let colors = owner_dir.join("colors");
    init_repo(&colors);
    write_manifest(&colors, "colors", "1.0.0", &[]);
    commit_all(&colors, "v1.0.0");
    tag(&colors, "v1.0.0");
    fs::write(
        colors.join("src/lib.spar"),
        "export var loaded: bool = false;",
    )
    .unwrap();
    write_manifest(&colors, "colors", "2.0.0", &[]);
    commit_all(&colors, "v2.0.0");
    tag(&colors, "v2.0.0");

    // Two packages pinning different versions of "colors".
    let app_a = owner_dir.join("app-a");
    init_repo(&app_a);
    write_manifest(
        &app_a,
        "app-a",
        "1.0.0",
        &[("colors", "github:owner/colors@1.0.0")],
    );
    commit_all(&app_a, "initial");
    tag(&app_a, "v1.0.0");

    let app_b = owner_dir.join("app-b");
    init_repo(&app_b);
    write_manifest(
        &app_b,
        "app-b",
        "1.0.0",
        &[("colors", "github:owner/colors@2.0.0")],
    );
    commit_all(&app_b, "initial");
    tag(&app_b, "v1.0.0");

    let root_manifest_src = format!(
        "[Package] {{\n    name: str = \"root\";\n    version: str = \"1.0.0\";\n    kind: str = \"config\";\n}};\n[Dependencies] {{\n    a: str = \"github:owner/app-a@1.0.0\";\n    b: str = \"github:owner/app-b@1.0.0\";\n}};\n",
    );
    let root_manifest =
        PackageManifest::parse(&root_manifest_src, &root.path().join("spar.package.spar")).unwrap();

    let provider = GitCommandProvider::with_remote_base(
        root.path().join("remote").to_string_lossy().into_owned(),
    );
    let resolver = DependencyResolver::new(&provider, NetworkPolicy::Allow);
    let graph = resolver.resolve(&root_manifest, root.path()).unwrap();

    let colors_nodes: Vec<_> = graph
        .nodes
        .values()
        .filter(|n| n.manifest.name == "colors")
        .collect();
    assert_eq!(
        colors_nodes.len(),
        2,
        "expected two distinct colors revisions"
    );
    assert_ne!(
        colors_nodes[0].manifest.version,
        colors_nodes[1].manifest.version
    );
}

#[test]
fn dependency_cycle_is_detected_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let a_dir = root.path().join("a");
    let b_dir = root.path().join("b");
    write_manifest(&a_dir, "a", "1.0.0", &[("b", "path:../b")]);
    write_manifest(&b_dir, "b", "1.0.0", &[("a", "path:../a")]);

    let root_manifest_src = "[Package] {\n    name: str = \"root\";\n    version: str = \"1.0.0\";\n    kind: str = \"config\";\n};\n[Dependencies] {\n    a: str = \"path:a\";\n};\n";
    let root_manifest =
        PackageManifest::parse(root_manifest_src, &root.path().join("spar.package.spar")).unwrap();

    let provider = PanicOnFetchProvider;
    let resolver = DependencyResolver::new(&provider, NetworkPolicy::Offline);
    let error = resolver
        .resolve(&root_manifest, root.path())
        .expect_err("a -> b -> a must be reported as a cycle");
    assert!(matches!(error, PackageError::Cycle { .. }));
    let message = error.to_string();
    assert!(message.contains('a') && message.contains('b'), "{message}");
}

#[test]
fn local_override_is_used_without_any_network_access() {
    let root = tempfile::tempdir().unwrap();
    let http_override = root.path().join("http-dev");
    write_manifest(&http_override, "http", "9.9.9", &[]);

    let root_manifest_src = "[Package] {\n    name: str = \"root\";\n    version: str = \"1.0.0\";\n    kind: str = \"config\";\n};\n[Dependencies] {\n    http: str = \"github:owner/http@1.0.0\";\n};\n[Overrides] {\n    http: str = \"path:http-dev\";\n};\n";
    let root_manifest =
        PackageManifest::parse(root_manifest_src, &root.path().join("spar.package.spar")).unwrap();

    // PanicOnFetchProvider proves the override short-circuited before any
    // github fetch was attempted.
    let provider = PanicOnFetchProvider;
    let resolver = DependencyResolver::new(&provider, NetworkPolicy::Allow);
    let graph = resolver.resolve(&root_manifest, root.path()).unwrap();

    assert_eq!(graph.nodes.len(), 1);
    let node = graph.nodes.values().next().unwrap();
    assert_eq!(node.manifest.name, "http");
    assert_eq!(node.manifest.version.to_string(), "9.9.9");
}

#[test]
fn offline_network_policy_rejects_a_github_fetch_before_touching_git() {
    let root = tempfile::tempdir().unwrap();
    let root_manifest_src = "[Package] {\n    name: str = \"root\";\n    version: str = \"1.0.0\";\n    kind: str = \"config\";\n};\n[Dependencies] {\n    http: str = \"github:owner/http@1.0.0\";\n};\n";
    let root_manifest =
        PackageManifest::parse(root_manifest_src, &root.path().join("spar.package.spar")).unwrap();

    let provider = GitCommandProvider::default();
    let resolver = DependencyResolver::new(&provider, NetworkPolicy::Offline);
    let error = resolver
        .resolve(&root_manifest, root.path())
        .expect_err("offline resolution of a github dependency must fail");
    assert!(matches!(error, PackageError::Network { .. }));
}

#[test]
fn resolves_a_branch_selector_to_its_current_commit() {
    let root = tempfile::tempdir().unwrap();
    let owner_dir = root.path().join("remote").join("owner");
    fs::create_dir_all(&owner_dir).unwrap();
    let repo = owner_dir.join("http");
    init_repo(&repo);
    write_manifest(&repo, "http", "1.0.0", &[]);
    commit_all(&repo, "initial");

    let root_manifest_src = "[Package] {\n    name: str = \"root\";\n    version: str = \"1.0.0\";\n    kind: str = \"config\";\n};\n[Dependencies] {\n    http: str = \"github:owner/http#main\";\n};\n";
    let root_manifest =
        PackageManifest::parse(root_manifest_src, &root.path().join("spar.package.spar")).unwrap();

    let provider = GitCommandProvider::with_remote_base(
        root.path().join("remote").to_string_lossy().into_owned(),
    );
    let resolver = DependencyResolver::new(&provider, NetworkPolicy::Allow);
    let graph = resolver.resolve(&root_manifest, root.path()).unwrap();
    assert_eq!(graph.nodes.len(), 1);
    let node = graph.nodes.values().next().unwrap();
    assert_eq!(node.manifest.name, "http");
    let spar::package::LockedSource::Github { revision, .. } = &node.locked.source else {
        panic!("expected a github source");
    };
    assert_eq!(
        revision.len(),
        40,
        "expected a full commit sha, got {revision}"
    );
}
