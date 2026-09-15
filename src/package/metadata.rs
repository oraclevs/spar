//! Built-in shapes and restricted validation for Spar's two reserved
//! package metadata files. These are compiler intrinsics, not stdlib APIs.

use std::path::Path;

use crate::ast::{Program, SparType, TopLevelItem, TypeDecl, TypeField, TypeFieldShape};
use crate::error::{Span, SparError};
use crate::package::{Lockfile, PackageManifest, PACKAGE_LOCK_FILE};

pub const PACKAGE_MANIFEST_FILE: &str = "spar.package.spar";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataFileKind {
    Manifest,
    Lock,
}

pub fn metadata_kind(path: &Path) -> Option<MetadataFileKind> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(PACKAGE_MANIFEST_FILE) => Some(MetadataFileKind::Manifest),
        Some(PACKAGE_LOCK_FILE) => Some(MetadataFileKind::Lock),
        _ => None,
    }
}

pub(crate) fn inject_builtin_types(program: &mut Program, kind: MetadataFileKind) {
    let types = match kind {
        MetadataFileKind::Manifest => vec![type_decl(
            "SparPackage",
            vec![
                primitive("name", false, SparType::Str),
                primitive("version", false, SparType::Str),
                primitive("kind", false, SparType::Str),
                primitive("entry", true, SparType::Str),
            ],
        )],
        MetadataFileKind::Lock => vec![
            type_decl(
                "SparLockedDependency",
                vec![
                    primitive("alias", false, SparType::Str),
                    primitive("packageId", false, SparType::Str),
                ],
            ),
            type_decl(
                "SparLockedPackage",
                vec![
                    primitive("id", false, SparType::Str),
                    primitive("name", false, SparType::Str),
                    primitive("version", false, SparType::Str),
                    primitive("sourceKind", false, SparType::Str),
                    primitive("sourceLocation", false, SparType::Str),
                    primitive("revision", false, SparType::Str),
                    primitive("integrity", false, SparType::Str),
                    primitive("entry", false, SparType::Str),
                    primitive(
                        "dependencies",
                        false,
                        SparType::List(Box::new(SparType::Named("SparLockedDependency".into()))),
                    ),
                ],
            ),
            type_decl(
                "SparPackageLock",
                vec![
                    primitive("formatVersion", false, SparType::Int),
                    primitive(
                        "root",
                        false,
                        SparType::List(Box::new(SparType::Named("SparLockedDependency".into()))),
                    ),
                    primitive(
                        "packages",
                        false,
                        SparType::List(Box::new(SparType::Named("SparLockedPackage".into()))),
                    ),
                ],
            ),
        ],
    };
    program
        .items
        .extend(types.into_iter().map(TopLevelItem::Type));
}

pub(crate) fn validate_source(
    source: &str,
    path: &Path,
    kind: MetadataFileKind,
) -> Result<(), SparError> {
    let result = match kind {
        MetadataFileKind::Manifest => PackageManifest::parse(source, path).map(|_| ()),
        MetadataFileKind::Lock => Lockfile::parse_spar(source, path).map(|_| ()),
    };
    result.map_err(|error| SparError::TypeError {
        message: error.to_string(),
        hint: None,
        span: Span::new(0, 0, 1, 1),
    })
}

fn type_decl(name: &str, fields: Vec<TypeField>) -> TypeDecl {
    let span = builtin_span();
    TypeDecl {
        name: name.into(),
        name_span: span.clone(),
        exported: false,
        fields,
        span,
    }
}

fn primitive(name: &str, optional: bool, ty: SparType) -> TypeField {
    TypeField {
        name: name.into(),
        optional,
        shape: TypeFieldShape::Primitive(ty),
        span: builtin_span(),
    }
}

fn builtin_span() -> Span {
    Span::new(0, 0, 1, 1)
}
