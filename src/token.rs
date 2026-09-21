use crate::error::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Var,
    KwMut,
    Export,
    Import,
    As,
    Dynamic,
    Private, // the keyword "private" as a section prefix

    // Type keywords
    TypeStr,
    TypeInt,
    TypeFloat,
    TypeBool,
    TypeSection, // the keyword "section" as a type annotation
    TypeVoid,    // the keyword "void" — only legal as a function return type
    TypeShell,

    // Boolean literals
    True,
    False,

    // Value literals
    IntLit(i64),
    FloatLit(f64),

    // String interpolation sequence
    StringStart,
    StringFragment(String),
    InterpolStart,
    InterpolEnd,
    CommandSubStart,
    CommandSubEnd,
    StringEnd,

    // Identifiers
    Ident(String),

    // Arithmetic operators
    Plus,
    PlusEq,
    Minus,
    Star,
    Slash,

    // Assignment and fallback
    Eq,
    QuestionQuestion,

    // Comparison + boolean operators
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    AndAnd,
    OrOr,
    StructuredPipe, // `|>` Spar structured-value pipe
    Bang,
    At, // `@`
    HashBracket, // `#[` opens an attribute

    // Function keywords
    KwAsync,
    KwAwait,
    KwFunction,
    KwFn,
    KwReturn,
    KwIf,
    KwElse,
    KwFor,
    KwIn,
    KwBreak,
    KwContinue,
    KwTry,
    KwCatch,
    KwStruct,
    KwImpl,
    KwCommand,
    KwExec,

    // Arrows
    Arrow,
    FatArrow,

    // Punctuation
    Colon,
    Semicolon,
    Question,
    Dot,
    Comma,
    DotDotDot,
    ColonColon,

    // Dedicated native shell-language tokens.
    ShellBlockStart,
    ShellForeignBlockStart(String),
    ShellBlockEnd,
    ShellWord(String),
    ShellLiteralWord(String),
    /// Raw Spar expression immediately following `|>` inside native shell syntax.
    /// The shell lexer preserves it as text so the ordinary Spar parser can
    /// parse/type-check the structured stage instead of treating it as argv.
    ShellStructuredStage(String),
    /// Raw decoder specification captured after a native `| from` bridge.
    ShellDecoderStage(String),
    ShellPipe,
    ShellRedirectAppend,
    ShellRedirectStderr,
    ShellFdRedirect {
        fd: u32,
        append: bool,
    },
    ShellFdDuplicate {
        fd: u32,
        target: u32,
    },
    ShellRedirectBoth {
        append: bool,
    },
    ShellBackground,

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    // Raw shell body of a task's `run { ... }` block. Emitted only when the
    // lexer sees `run` immediately followed by `{` (see `Lexer::maybe_enter_run_body`).
    RunStart,
    ShellFragment(String),
    RunEnd,

    Eof,
}

#[derive(Debug, Clone)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Span,
}

impl SpannedToken {
    pub fn new(token: Token, span: Span) -> Self {
        Self { token, span }
    }

    pub fn is(&self, other: &Token) -> bool {
        &self.token == other
    }
}

impl Token {
    pub fn human_name(&self) -> &'static str {
        match self {
            Token::Semicolon => "';'",
            Token::Colon => "':'",
            Token::Comma => "','",
            Token::Dot => "'.'",
            Token::Eq => "'='",
            Token::LBrace => "'{'",
            Token::RBrace => "'}'",
            Token::LBracket => "'['",
            Token::RBracket => "']'",
            Token::LParen => "'('",
            Token::RParen => "')'",
            Token::Question => "'?'",
            Token::QuestionQuestion => "'??'",
            Token::DotDotDot => "'...'",
            Token::ColonColon => "'::'",
            Token::Plus => "'+'",
            Token::PlusEq => "'+='",
            Token::Minus => "'-'",
            Token::Star => "'*'",
            Token::Slash => "'/'",
            Token::Var => "'var'",
            Token::KwMut => "'mut'",
            Token::Export => "'export'",
            Token::Import => "'import'",
            Token::As => "'as'",
            Token::Dynamic => "'dynamic'",
            Token::Private => "'private'",
            Token::True => "'true'",
            Token::False => "'false'",
            Token::TypeInt => "'int'",
            Token::TypeFloat => "'float'",
            Token::TypeStr => "'str'",
            Token::TypeBool => "'bool'",
            Token::TypeSection => "'section'",
            Token::TypeVoid => "'void'",
            Token::TypeShell => "'shell'",
            Token::EqEq => "'=='",
            Token::NotEq => "'!='",
            Token::Lt => "'<'",
            Token::Gt => "'>'",
            Token::LtEq => "'<='",
            Token::GtEq => "'>='",
            Token::AndAnd => "'&&'",
            Token::OrOr => "'||'",
            Token::Bang => "'!'",
            Token::At => "'@'",
            Token::HashBracket => "'#['",
            Token::KwAsync => "'async'",
            Token::KwAwait => "'await'",
            Token::KwFunction => "'function'",
            Token::KwFn => "'fn'",
            Token::KwReturn => "'return'",
            Token::KwIf => "'if'",
            Token::KwElse => "'else'",
            Token::KwFor => "'for'",
            Token::KwIn => "'in'",
            Token::KwBreak => "'break'",
            Token::KwContinue => "'continue'",
            Token::KwTry => "'try'",
            Token::KwCatch => "'catch'",
            Token::KwStruct => "'struct'",
            Token::KwImpl => "'impl'",
            Token::KwCommand => "'command'",
            Token::KwExec => "'exec'",
            Token::Arrow => "'->'",
            Token::FatArrow => "'=>'",
            Token::StructuredPipe => "'|>'",
            Token::Ident(_) => "identifier",
            Token::StringStart => "string",
            Token::StringFragment(_) => "string content",
            Token::StringEnd => "end of string",
            Token::InterpolStart => "'${'",
            Token::InterpolEnd => "'}'",
            Token::CommandSubStart => "'$('",
            Token::CommandSubEnd => "')'",
            Token::IntLit(_) => "integer literal",
            Token::FloatLit(_) => "float literal",
            Token::RunStart => "'{'",
            Token::ShellFragment(_) => "shell text",
            Token::RunEnd => "end of run block",
            Token::ShellBlockStart => "'shell {'",
            Token::ShellForeignBlockStart(_) => "foreign shell block",
            Token::ShellBlockEnd => "end of shell block",
            Token::ShellWord(_) => "shell word",
            Token::ShellLiteralWord(_) => "literal shell word",
            Token::ShellStructuredStage(_) => "structured shell stage",
            Token::ShellDecoderStage(_) => "shell decoder stage",
            Token::ShellPipe => "'|'",
            Token::ShellRedirectAppend => "'>>'",
            Token::ShellRedirectStderr => "'2>'",
            Token::ShellFdRedirect { .. } => "file-descriptor redirect",
            Token::ShellFdDuplicate { .. } => "file-descriptor duplication",
            Token::ShellRedirectBoth { append: false } => "'&>'",
            Token::ShellRedirectBoth { append: true } => "'&>>'",
            Token::ShellBackground => "'&'",
            Token::Eof => "end of file",
        }
    }
}

pub fn keyword_or_ident(s: String) -> Token {
    match s.as_str() {
        "var" => Token::Var,
        "mut" => Token::KwMut,
        "export" => Token::Export,
        "import" => Token::Import,
        "as" => Token::As,
        "dynamic" => Token::Dynamic,
        "private" => Token::Private,
        "str" => Token::TypeStr,
        "int" => Token::TypeInt,
        "float" => Token::TypeFloat,
        "bool" => Token::TypeBool,
        "section" => Token::TypeSection,
        "void" => Token::TypeVoid,
        "shell" => Token::TypeShell,
        "true" => Token::True,
        "false" => Token::False,
        "async" => Token::KwAsync,
        "await" => Token::KwAwait,
        "function" => Token::KwFunction,
        "fn" => Token::KwFn,
        "return" => Token::KwReturn,
        "if" => Token::KwIf,
        "else" => Token::KwElse,
        "for" => Token::KwFor,
        "in" => Token::KwIn,
        "break" => Token::KwBreak,
        "continue" => Token::KwContinue,
        "try" => Token::KwTry,
        "catch" => Token::KwCatch,
        "struct" => Token::KwStruct,
        "impl" => Token::KwImpl,
        "command" => Token::KwCommand,
        "exec" => Token::KwExec,
        _ => Token::Ident(s),
    }
}
