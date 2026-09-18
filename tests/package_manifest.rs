use std::path::Path;

use spar::package::{PackageKind, PackageManifest};

const MANIFEST_WITH_OVERRIDE: &str = r#"
[Package] {
    name: str = "my-app";
    version: str = "1.0.0";
    kind: str = "application";
    entry: str = "app/start.spar";
};

[Dependencies] {
    http: str = "github:owner/http@1.4.0";
};

[Overrides] {
    http: str = "path:../http";
};
"#;

#[test]
fn application_manifest_uses_explicit_entry_and_keeps_override_separate() {
    let manifest =
        PackageManifest::parse(MANIFEST_WITH_OVERRIDE, Path::new("spar.package.spar")).unwrap();
    assert_eq!(manifest.name, "my-app");
    assert_eq!(manifest.kind, PackageKind::Application);
    assert_eq!(manifest.entry, Path::new("app/start.spar"));
    assert_eq!(manifest.dependencies["http"], "github:owner/http@1.4.0");
    assert_eq!(manifest.overrides["http"], "path:../http");
}

#[test]
fn conventional_entry_is_derived_from_kind_when_entry_is_omitted() {
    let src = r#"
        [Package] {
            name: str = "mylib";
            version: str = "0.1.0";
            kind: str = "library";
        };
    "#;
    let manifest = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap();
    assert_eq!(manifest.entry, Path::new("src/lib.spar"));

    let src = r#"
        [Package] {
            name: str = "myconfig";
            version: str = "0.1.0";
            kind: str = "config";
        };
    "#;
    let manifest = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap();
    assert_eq!(manifest.entry, Path::new("src/config.spar"));
}

const MANIFESTS_WITH_RUNTIME_CONSTRUCTS: &[&str] = &[
    // interpolation
    r#"
    [Package] {
        name: str = "x";
        version: str = "1.0.0";
        kind: str = "config";
    };
    var suffix: str = "-dev";
    [Dependencies] {
        http: str = "github:owner/http@1.0.0${suffix}";
    };
    "#,
    // a top-level function declaration alongside the sections
    r#"
    function helper() -> int { return 1; };
    [Package] {
        name: str = "x";
        version: str = "1.0.0";
        kind: str = "config";
    };
    "#,
];

#[test]
fn manifest_rejects_runtime_constructs_without_executing_them() {
    for source in MANIFESTS_WITH_RUNTIME_CONSTRUCTS {
        let error = PackageManifest::parse(source, Path::new("spar.package.spar"))
            .expect_err("manifest with a runtime construct must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("literal") || message.contains("manifest may contain only"),
            "{message}"
        );
    }
}

#[test]
fn missing_required_package_fields_are_reported() {
    let error = PackageManifest::parse(
        r#"[Package] { name: str = "x"; };"#,
        Path::new("spar.package.spar"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("version"), "{error}");
}

#[test]
fn invalid_semver_version_is_reported() {
    let src = r#"
        [Package] {
            name: str = "x";
            version: str = "not-a-version";
            kind: str = "config";
        };
    "#;
    let error = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap_err();
    assert!(error.to_string().contains("SemVer"), "{error}");
}

#[test]
fn invalid_kind_is_reported() {
    let src = r#"
        [Package] {
            name: str = "x";
            version: str = "1.0.0";
            kind: str = "daemon";
        };
    "#;
    let error = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap_err();
    assert!(error.to_string().contains("kind"), "{error}");
}

#[test]
fn override_naming_an_undeclared_dependency_is_rejected() {
    let src = r#"
        [Package] {
            name: str = "x";
            version: str = "1.0.0";
            kind: str = "config";
        };
        [Overrides] {
            http: str = "path:../http";
        };
    "#;
    let error = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap_err();
    assert!(error.to_string().contains("http"), "{error}");
}

#[test]
fn invalid_dependency_request_syntax_is_reported_at_parse_time() {
    let src = r#"
        [Package] {
            name: str = "x";
            version: str = "1.0.0";
            kind: str = "config";
        };
        [Dependencies] {
            http: str = "not-a-valid-request";
        };
    "#;
    let error = PackageManifest::parse(src, Path::new("spar.package.spar")).unwrap_err();
    assert!(
        error.to_string().contains("invalid dependency request"),
        "{error}"
    );
}

#[test]
fn std_dependency_alias_is_reserved() {
    let source = r#"
        struct Package: SparPackage {
            name = "app";
            version = "1.0.0";
            kind = "application";
        };
        struct Dependencies {
            std: str = "path:../std";
        };
    "#;
    let error = PackageManifest::parse(source, Path::new("spar.package.spar")).unwrap_err();
    assert!(error.to_string().contains("reserved"), "{error}");
    assert!(error.to_string().contains("std"), "{error}");
}
