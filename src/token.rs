use crate::error::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Var,
    Export,
    Import,
    As,
    Dynamic,
    Private,      // the keyword "private" as a section prefix

    // Type keywords
    TypeStr,
    TypeInt,
    TypeFloat,
    TypeBool,
    TypeSection,    // the keyword "section" as a type annotation

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
    StringEnd,

    // Identifiers
    Ident(String),

    // Arithmetic operators
    Plus,
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
    Bang,
    At,             // `@`

    // Function keywords
    KwFunction,
    KwReturn,
    KwIf,
    KwElse,
    KwFor,
    KwIn,

    // Arrow
    Arrow,

    // Punctuation
    Colon,
    Semicolon,
    Question,
    Dot,
    Comma,
    DotDotDot,
    ColonColon,

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

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
            Token::Semicolon         => "';'",
            Token::Colon             => "':'",
            Token::Comma             => "','",
            Token::Dot               => "'.'",
            Token::Eq                => "'='",
            Token::LBrace            => "'{'",
            Token::RBrace            => "'}'",
            Token::LBracket          => "'['",
            Token::RBracket          => "']'",
            Token::LParen            => "'('",
            Token::RParen            => "')'",
            Token::Question          => "'?'",
            Token::QuestionQuestion  => "'??'",
            Token::DotDotDot         => "'...'",
            Token::ColonColon        => "'::'",
            Token::Plus              => "'+'",
            Token::Minus             => "'-'",
            Token::Star              => "'*'",
            Token::Slash             => "'/'",
            Token::Var               => "'var'",
            Token::Export            => "'export'",
            Token::Import            => "'import'",
            Token::As                => "'as'",
            Token::Dynamic           => "'dynamic'",
            Token::Private           => "'private'",
            Token::True              => "'true'",
            Token::False             => "'false'",
            Token::TypeInt           => "'int'",
            Token::TypeFloat         => "'float'",
            Token::TypeStr           => "'str'",
            Token::TypeBool          => "'bool'",
            Token::TypeSection       => "'section'",
            Token::EqEq              => "'=='",
            Token::NotEq             => "'!='",
            Token::Lt                => "'<'",
            Token::Gt                => "'>'",
            Token::LtEq              => "'<='",
            Token::GtEq              => "'>='",
            Token::AndAnd             => "'&&'",
            Token::OrOr              => "'||'",
            Token::Bang              => "'!'",
            Token::At                => "'@'",
            Token::KwFunction        => "'function'",
            Token::KwReturn          => "'return'",
            Token::KwIf              => "'if'",
            Token::KwElse            => "'else'",
            Token::KwFor             => "'for'",
            Token::KwIn              => "'in'",
            Token::Arrow             => "'->'",
            Token::Ident(_)          => "identifier",
            Token::StringStart       => "string",
            Token::StringFragment(_) => "string content",
            Token::StringEnd         => "end of string",
            Token::InterpolStart     => "'${'",
            Token::InterpolEnd       => "'}'",
            Token::IntLit(_)         => "integer literal",
            Token::FloatLit(_)       => "float literal",
            Token::Eof               => "end of file",

        }
    }
}

pub fn keyword_or_ident(s: String) -> Token {
    match s.as_str() {
        "var"      => Token::Var,
        "export"   => Token::Export,
        "import"   => Token::Import,
        "as"       => Token::As,
        "dynamic"  => Token::Dynamic,
        "private"  => Token::Private,
        "str"      => Token::TypeStr,
        "int"      => Token::TypeInt,
        "float"    => Token::TypeFloat,
        "bool"     => Token::TypeBool,
        "section"  => Token::TypeSection,
        "true"     => Token::True,
        "false"    => Token::False,
        "function" => Token::KwFunction,
        "return"   => Token::KwReturn,
        "if"       => Token::KwIf,
        "else"     => Token::KwElse,
        "for"      => Token::KwFor,
        "in"       => Token::KwIn,
        _          => Token::Ident(s),
    }
}
