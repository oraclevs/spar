//! Editor intelligence shared by `spar-ls` and `sparsh`.
//!
//! This module must stay free of LSP and async-runtime dependencies: it works
//! on plain source text, byte offsets and paths.

pub mod chain;
pub mod cursor;
pub mod exports;
pub mod member;

pub use exports::{exports_of, exports_of_with_budget, import_context};
pub use member::complete_members;

/// What kind of declaration an import candidate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    /// A `function` declaration.
    Function,
    /// An `export var` whose type is a function type.
    Callable,
    Variable,
    Struct,
    Enum,
    Type,
    Group,
}

/// A name a file offers to `import { }`.
#[derive(Debug, Clone)]
pub struct ExportItem {
    pub name: String,
    pub kind: ExportKind,
    /// Type or signature label.
    pub detail: Option<String>,
    /// Leading `///` documentation.
    pub doc: Option<String>,
    /// Declaration span in the file that owns it.
    pub span: crate::error::Span,
    /// Resolved file that owns the declaration.
    pub file: std::path::PathBuf,
}

/// The module named by the `from` clause of an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// `import { } from "./lib.spar"`.
    File(String),
    /// `import pkg { } from "std/fs"`.
    Package(String),
    /// No `from` clause has been typed yet.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntelError {
    NotFound(String),
    Parse(String),
    Timeout,
}

impl std::fmt::Display for IntelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntelError::NotFound(message) => write!(f, "not found: {message}"),
            IntelError::Parse(message) => write!(f, "parse error: {message}"),
            IntelError::Timeout => write!(f, "timed out"),
        }
    }
}

impl std::error::Error for IntelError {}

/// The cursor sits between the braces of an `import { }` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCursor {
    pub target: ImportTarget,
    /// Names already listed (the word under the cursor excluded).
    pub already: std::collections::HashSet<String>,
    /// First word of the comma segment holding the cursor (`b` in `b as |`),
    /// when that segment has one. It is not part of `already`.
    pub current: Option<String>,
    /// The partial word before the cursor.
    pub typed: String,
    /// Byte offset where `typed` starts.
    pub replace_start: usize,
    pub type_only: bool,
    /// `import pkg { }`.
    pub package: bool,
    /// Byte offset of the closing `}`, when one exists.
    pub close_at: Option<usize>,
}

/// A completion or signature query against a source buffer.
#[derive(Debug, Clone, Copy)]
pub struct IntelRequest<'a> {
    pub source: &'a str,
    /// Byte offset of the cursor in `source`.
    pub offset: usize,
    /// Directory relative imports resolve against.
    pub base_dir: &'a std::path::Path,
}

/// What a completion item stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntelKind {
    Function,
    Method,
    Variable,
    Field,
    Struct,
    Enum,
    EnumMember,
    Type,
    Keyword,
    Parameter,
    Module,
}

/// One completion candidate, free of any editor protocol type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntelItem {
    pub label: String,
    pub kind: IntelKind,
    pub detail: Option<String>,
    /// Text to insert; empty means "insert the label".
    pub insert_text: String,
    pub doc: Option<String>,
    /// Ordering key an editor sorts by; `None` leaves the order to the caller.
    pub sort_text: Option<String>,
}
