use crate::error::Span;

#[derive(Debug, Clone)]
pub struct Program {
    pub is_schema_file: bool,
    pub load_env: Option<String>,
    pub items: Vec<TopLevelItem>,
    /// A leading `#!...` line, if the source had one. Not produced by the
    /// parser itself — the lexer skips it as trivia; callers that care
    /// about round-tripping it read it off the `Lexer` before tokenizing
    /// and set it on the returned `Program`.
    pub shebang: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TopLevelItem {
    Import(ImportDecl),
    Var(VarDecl),
    Dynamic(DynamicDecl),
    Section(SectionDecl),
    Function(FunctionDecl),
    SchemaSection(SchemaSectionDecl),
    Type(TypeDecl),
    SchemaFrom(SchemaFromDecl),
    Enum(EnumDecl),
    FunctionGroup(FunctionGroupDecl),
    Task(Box<TaskDecl>),
    Statement(Statement),
}

/// A `task Name(params) { ... }` declaration. Metadata fields
/// (`description`, `default`, `quiet`, `cwd`, `env`) are ordinary Spar
/// expressions, evaluated by `task_lowering` the same way any other Spar
/// value is. `depends_on` holds bare task-name references — tasks live in
/// their own namespace, not the general expression/symbol namespace, so
/// they are captured as plain names rather than `NamespaceRef`s.
#[derive(Debug, Clone)]
pub struct TaskDecl {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<TaskParam>,
    pub description: Option<Expr>,
    pub default: Option<Expr>,
    pub quiet: Option<Expr>,
    pub private: Option<Expr>,
    pub group: Option<Expr>,
    pub confirm: Option<Expr>,
    pub depends_on: Vec<TaskRef>,
    pub env: Vec<(String, Expr)>,
    pub cwd: Option<Expr>,
    pub run_blocks: Vec<RunBlock>,
    pub span: Span,
    /// The source line of each metadata field (everything but `run`), in
    /// the order they appeared — lets the formatter flush standalone
    /// comments at the right point inside the task body even though these
    /// fields are re-printed in a fixed canonical order.
    pub field_spans: Vec<(String, Span)>,
    /// The closing `}`'s span, so the formatter can flush any comment
    /// trailing the last field/run block without leaking it outside the
    /// task body.
    pub closing_span: Span,
}

/// A task parameter carries task-runner-specific call-site behaviour.
/// Function parameters deliberately remain the simpler `Param` shape.
#[derive(Debug, Clone)]
pub struct TaskParam {
    pub name: String,
    pub ty: SparType,
    pub default: Option<Expr>,
    pub variadic: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TaskRef {
    pub name: String,
    pub span: Span,
}

/// One shell invocation inside a `run { ... }` block — the raw text between
/// two top-level (not inside a shell quote) `;` separators, split at parse
/// time so the runner can echo/fail commands individually and preserve
/// declaration order (see `runner::TaskCommand`).
#[derive(Debug, Clone)]
pub struct ShellCommand {
    pub parts: Vec<ShellTemplatePart>,
    pub is_shebang: bool,
    pub span: Span,
}

/// Which language a `run` body is written in. `Spar` (the default) is the
/// native `shell {}` language; `Bash` is raw text handed to bash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunShell {
    Spar,
    Bash,
}

#[derive(Debug, Clone)]
pub enum RunBody {
    Native(ShellExpr),
    Bash(Vec<ShellCommand>),
}

/// One `run [spar|bash] [os] { ... }` clause. `os: None` is the any-OS
/// fallback. A task has at most one block per OS slot regardless of shell
/// (enforced by the parser).
#[derive(Debug, Clone)]
pub struct RunBlock {
    pub shell: RunShell,
    pub shell_span: Option<Span>,
    pub os: Option<String>,
    pub os_span: Option<Span>,
    pub body: RunBody,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ShellTemplatePart {
    /// Raw shell text, copied verbatim (quotes, pipes, redirects, braces).
    Literal(String),
    /// A `${expr}` interpolation island — an ordinary Spar expression parsed
    /// with the normal expression grammar. May be a bare identifier naming a
    /// task parameter (left as a neutral slot by `task_lowering`) or any
    /// other Spar expression (pre-evaluated by `task_lowering`).
    Expr(Expr),
}

/// A native command-language expression. This is not the same mechanism as
/// `ShellCommand`/`ShellTemplatePart` above — see the design doc's "Why this
/// needs a dedicated sub-grammar" section.
#[derive(Debug, Clone)]
pub struct ShellExpr {
    /// Ordinary Spar statements retained for deferred execution. Native
    /// commands inside a mixed block are represented as shell-valued
    /// expression statements, so nested `if`/`for` reuse the normal AST.
    pub statements: Vec<Statement>,
    pub steps: Vec<(ShellJoin, ShellStep)>,
    pub span: Span,
    pub foreign_shell: Option<String>,
    /// Source line of the closing token, so the formatter keeps comments
    /// trailing the last statement inside the block.
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub enum ShellJoin {
    Always,
    OnSuccess,
    OnFailure,
}

#[derive(Debug, Clone)]
pub enum ShellStep {
    Command(Box<ShellCommandExpr>),
    Pipeline(Vec<ShellCommandExpr>),
}

#[derive(Debug, Clone)]
pub struct ShellCommandExpr {
    pub environment: Vec<ShellEnvironmentEntry>,
    pub program: ShellWord,
    pub args: Vec<ShellWord>,
    pub stdin: Option<ShellRedirect>,
    pub stdout: Option<ShellRedirect>,
    pub stderr: Option<ShellRedirect>,
    pub redirections: Vec<ShellFdRedirect>,
    pub background: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ShellFdRedirect {
    pub fd: u32,
    pub target: ShellFdRedirectTarget,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ShellFdRedirectTarget {
    File(ShellRedirect),
    Duplicate(u32),
}

#[derive(Debug, Clone)]
pub struct ShellEnvironmentEntry {
    pub name: String,
    pub value: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ShellWord {
    pub text: String,
    pub parts: Vec<ShellWordPart>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ShellWordPart {
    Literal(String),
    Expr(Expr),
    Environment(String),
    /// Native command substitution embedded in one argv word, e.g.
    /// `"prefix-$(printf value)"`. The result is UTF-8 text and is
    /// appended to the surrounding word without implicit splitting.
    CommandSubstitution(ShellExpr),
}

#[derive(Debug, Clone)]
pub struct ShellRedirect {
    pub target: ShellWord,
    pub mode: spar_command::RedirectMode,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub name_span: Span,
    pub exported: bool,
    pub variants: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: String,
    pub name_span: Span,
    pub type_parameters: Vec<TypeParameter>,
    pub exported: bool,
    pub fields: Vec<TypeField>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TypeField {
    pub name: String,
    pub optional: bool,
    pub shape: TypeFieldShape,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeFieldShape {
    Primitive(SparType),
    Named(String),
    TypeParameter(String),
    Applied {
        name: String,
        arguments: Vec<SparType>,
    },
    Section(Vec<TypeField>),
}

#[derive(Debug, Clone)]
pub struct TypeBinding {
    pub ty: SparType,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeParameter {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImportItem {
    pub name: String,
    pub name_span: Span,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ImportKind {
    /// `import "path" as alias;` — `None` means derive the alias from the
    /// path's file stem, exactly as today.
    Aliased(Option<String>),
    /// `import schema "path";`
    Schema,
    /// `import { A, B as C } from "path";`
    Selective(Vec<ImportItem>),
    /// `import type { A, B } from "path";`
    TypeSelective(Vec<ImportItem>),
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub path: String,
    /// Explicit package-namespace import (`import pkg ...`). When false,
    /// the path is resolved strictly as a local/module import.
    pub package: bool,
    pub kind: ImportKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SchemaMarker {
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub struct SchemaField {
    pub name: String,
    pub optional: bool,
    pub shape: SchemaFieldShape,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum SchemaFieldShape {
    Primitive(SparType),
    Section(Vec<SchemaField>),
}

#[derive(Debug, Clone)]
pub struct SchemaSectionDecl {
    pub name: String,
    pub marker: SchemaMarker,
    pub fields: Vec<SchemaField>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SchemaFromDecl {
    pub name: String,
    pub source_type: String,
    pub source_type_span: Span,
    pub marker: SchemaMarker,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub exported: bool,
    pub mutable: bool,
    pub name: String,
    pub optional: bool,
    pub ty: SparType,
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct DynamicDecl {
    pub name: String,
    pub optional: bool,
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SectionDecl {
    pub exported: bool,
    pub private: bool,
    /// True when source used canonical `struct`; false means legacy section syntax.
    pub canonical: bool,
    pub path: Vec<String>,
    pub items: Vec<SectionItem>,
    pub type_binding: Option<TypeBinding>,
    pub span: Span,
    /// Source line of the closing `}`, so the formatter can keep comments
    /// trailing the last field inside the body.
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub enum SectionItem {
    Field(FieldDecl),
    Spread(SpreadStmt),
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: String,
    pub optional: bool,
    /// `None` when the type is inferred from the enclosing section's
    /// `-> TypeName` binding (`name: value;`, no explicit type). Always
    /// `Some` in a section with no binding — the typechecker enforces
    /// that, not the parser.
    pub ty: Option<SparType>,
    pub value: Option<FieldValue>,
    pub span: Span,
    /// Source line of the closing `}` for a nested-section value, else the
    /// field's own line — lets the formatter keep comments inside the body.
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub struct SpreadStmt {
    pub expr: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SparType {
    Str,
    Int,
    Float,
    Bool,
    Section, // inline nested section body
    /// Function return type only — the parser never accepts `void` for a
    /// var, param, field, or list element type, so this variant can't
    /// reach storage positions.
    Void,
    Shell,
    Error,
    List(Box<SparType>),
    Named(String), // a declared `type X { ... }`, referenced by name
    TypeParameter(String),
    Applied {
        name: String,
        arguments: Vec<SparType>,
    },
}

/// The right-hand side of a field declaration.
#[derive(Debug, Clone)]
pub enum FieldValue {
    /// Normal expression (int, float, str, bool, list fields).
    Expr(Expr),
    /// Inline nested section body (only for section-type fields). Reuses
    /// `SectionItem` (not a bare `Vec<FieldDecl>`) so a nested body can
    /// also contain `...SourceSection;` spreads, same as a top-level
    /// section body already can.
    Nested(Vec<SectionItem>),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    String(InterpolString),
    NamespaceRef(NamespaceRef),
    FnCall(FnCall),
    BinaryOp(BinaryOp),
    List(Vec<Expr>, Span),
    Grouped(Box<Expr>, Span),
    Call {
        name: String,
        name_span: Span,
        type_arguments: Vec<SparType>,
        args: Vec<CallArg>,
        span: Span,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
        span: Span,
    },
    Await {
        value: Box<Expr>,
        span: Span,
    },
    Comprehension {
        var_name: String,
        var_name_span: Span,
        source: Box<Expr>,
        body: Box<Expr>,
        span: Span,
    },
    Index {
        source: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    FieldAccess {
        base: Box<Expr>,
        field: String,
        field_span: Span,
        span: Span,
    },
    /// An anonymous object literal — `{ field: value; ...Spread; }`. Reuses
    /// `SectionItem` verbatim, the same Field/Spread payload a nested
    /// section body (`FieldValue::Nested`) already carries. Only reachable
    /// via general expression parsing (list elements, var values, call
    /// args, ...) — a `{` appearing as a FIELD's own value is still always
    /// captured as `FieldValue::Nested` by `parse_field_decl`, never as
    /// this variant.
    Object(Vec<SectionItem>, Span),
    Shell(ShellExpr),
    ExecShell(ShellExpr),
    CommandSubstitution(ShellExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Fallback,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone)]
pub struct BinaryOp {
    pub op: BinOp,
    pub lhs: Box<Expr>,
    pub rhs: Box<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
}

#[derive(Debug, Clone)]
pub struct InterpolString {
    pub parts: Vec<StringPart>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StringPart {
    Literal(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct NamespaceRef {
    pub segments: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnCall {
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CallArg {
    pub param_name: String,
    pub param_name_span: Span,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub name: String,
    pub name_span: Span,
    pub type_parameters: Vec<TypeParameter>,
    pub params: Vec<Param>,
    pub ret: SparType,
    pub ret_span: Span,
    pub body: FunctionBody,
    pub is_async: bool,
    pub is_private: bool,
    /// Compiler-owned trust marker. Source parsing always sets this false;
    /// the loader marks functions originating from the bundled std package
    /// so only trusted library code can call private native capabilities.
    pub trusted_native: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionGroupDecl {
    pub is_private: bool,
    pub name: String,
    pub name_span: Span,
    pub functions: Vec<FunctionDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: SparType,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionBody {
    pub stmts: Vec<FuncStmt>,
    pub span: Span,
}

/// One executable Spar statement.
///
/// Function bodies currently expose the historical `FuncStmt` alias below;
/// module and nested block support can therefore grow around one statement
/// representation without duplicating control-flow nodes.
#[derive(Debug, Clone)]
pub enum Statement {
    LocalVar(LocalVarDecl),
    Assignment {
        name: String,
        value: Expr,
        span: Span,
    },
    Expression(Expr, Span),
    If(IfStmt),
    Return(ReturnValue, Span),
    For(ForStmt),
    Break(Span),
    Continue(Span),
    Try(TryStmt),
}

/// Backward-compatible name retained for embedders that inspect the public AST.
pub type FuncStmt = Statement;

#[derive(Debug, Clone)]
pub struct TryStmt {
    pub body: Vec<Statement>,
    pub catch_name: Option<String>,
    pub catch_span: Span,
    pub handler: Vec<Statement>,
    pub span: Span,
    /// Source line of the `catch` handler's closing `}`.
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub struct ForStmt {
    pub binding: ForBinding,
    pub iterable: Expr,
    pub body: Vec<Statement>,
    pub span: Span,
    /// Source line of the closing `}`.
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub enum ForBinding {
    Value {
        name: String,
        span: Span,
    },
    Indexed {
        index_name: String,
        index_span: Span,
        value_name: String,
        value_span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct LocalVarDecl {
    pub name: String,
    pub mutable: bool,
    pub ty: Option<SparType>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct IfStmt {
    pub condition: Expr,
    pub then_stmts: Vec<FuncStmt>,
    pub else_stmts: Vec<FuncStmt>,
    pub span: Span,
    /// Source line of the `}` closing the `then` block (the `else` line
    /// when there is an else branch).
    pub then_end_line: u32,
    /// Source line of the last closing `}`.
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub enum ReturnValue {
    /// Bare `return;` — only legal inside a `-> void` function.
    Void,
    Expr(Expr),
    SectionBlock(Vec<ReturnField>),
}

#[derive(Debug, Clone)]
pub struct ReturnField {
    pub name: String,
    pub ty: Option<SparType>,
    pub value: Expr,
    pub span: Span,
}
