use spar::package::manifest::PackageManifest;
use std::path::Path;

#[test]
fn unified_struct_manifest_round_trips_typed_literal_fields() {
    let source = r#"struct Package {
        name: str = "atlas";
        version: str = "0.1.0";
        kind: str = "application";
        entry: str = "src/main.spar";
    };"#;
    let path = Path::new("spar.package.spar");
    let manifest = PackageManifest::parse(source, path).unwrap();
    let rendered = manifest.render();
    let parsed = PackageManifest::parse(&rendered, path).unwrap();
    assert_eq!(parsed.name, "atlas");
    assert_eq!(parsed.entry, manifest.entry);
}

#[test]
fn unified_struct_manifest_rejects_nonliteral_untyped_and_generic_fields() {
    for source in [
        "struct Package<T> { name: str = \"atlas\"; version: str = \"0.1.0\"; kind: str = \"application\"; };",
        "struct Package { name: int = \"atlas\"; version: str = \"0.1.0\"; kind: str = \"application\"; };",
        "struct Package { name: str = panic(message: \"must not execute\"); };",
    ] {
        assert!(PackageManifest::parse(source, Path::new("spar.package.spar")).is_err(), "{source}");
    }
}

#[test]
fn unified_struct_lockfile_round_trip_uses_current_syntax() {
    let lock = spar::package::Lockfile::default();
    let source = lock.to_spar().unwrap();
    spar::package::Lockfile::parse_spar(&source, Path::new("spar.package.lock.spar")).unwrap();
    assert!(source.contains("formatVersion: int ="));
}
