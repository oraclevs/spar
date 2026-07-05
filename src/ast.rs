use crate::error::Span;

#[derive(Debug, Clone)]
pub struct Program {
    pub is_schema_file: bool,
    pub items: Vec<TopLevelItem>,
}

#[derive(Debug, Clone)]
pub enum TopLevelItem {
    Import(ImportDecl),
    Var(VarDecl),
    Dynamic(DynamicDecl),
    Section(SectionDecl),
    Function(FunctionDecl),
    SchemaSection(SchemaSectionDecl),
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub path: String,
    pub alias: Option<String>,
    pub is_schema: bool,
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
pub struct VarDecl {
    pub exported: bool,
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
    pub path: Vec<String>,
    pub items: Vec<SectionItem>,
    pub span: Span,
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
    pub ty: SparType,
    pub value: Option<FieldValue>,
    pub span: Span,
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
    Section,                // inline nested section body
    List(Box<SparType>),
}

/// The right-hand side of a field declaration.
#[derive(Debug, Clone)]
pub enum FieldValue {
    /// Normal expression (int, float, str, bool, list fields).
    Expr(Expr),
    /// Inline nested section body (only for section-type fields).
    Nested(Vec<FieldDecl>),
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
        args: Vec<CallArg>,
        span: Span,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
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
    pub params: Vec<Param>,
    pub ret: SparType,
    pub ret_span: Span,
    pub body: FunctionBody,
    pub is_private: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: SparType,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionBody {
    pub stmts: Vec<FuncStmt>,
    pub span:  Span,
}

#[derive(Debug, Clone)]
pub enum FuncStmt {
    LocalVar(LocalVarDecl),
    If(IfStmt),
    Return(ReturnValue, Span),
    For {
        var_name: String,
        iterable: Expr,
        body: Vec<FuncStmt>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct LocalVarDecl {
    pub name: String,
    pub ty: SparType,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct IfStmt {
    pub condition: Expr,
    pub then_stmts: Vec<FuncStmt>,
    pub else_stmts: Vec<FuncStmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ReturnValue {
    Expr(Expr),
    SectionBlock(Vec<ReturnField>),
}

#[derive(Debug, Clone)]
pub struct ReturnField {
    pub name: String,
    pub ty: SparType,
    pub value: Expr,
    pub span: Span,
}
