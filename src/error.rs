#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, line: u32, col: u32) -> Self {
        Self { start, end, line, col }
    }

    pub fn dummy() -> Self {
        Self { start: 0, end: 0, line: 0, col: 0 }
    }
}

#[derive(Debug)]
pub enum SparError {
    LexError {
        message: String,
        span: Span,
    },
    ParseError {
        message: String,
        span: Span,
    },
    ResolveError {
        message: String,
        hint: Option<String>,
        span: Span,
    },
    TypeError {
        message: String,
        hint: Option<String>,
        span: Span,
    },
    EvalError {
        message: String,
        span: Span,
    },
    SchemaError {
        message: String,
        span: Span,
    },
}

impl std::fmt::Display for SparError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SparError::LexError { message, span } => {
                write!(f, "error[lex] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::ParseError { message, span } => {
                write!(f, "error[parse] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::ResolveError { message, span, .. } => {
                write!(f, "error[resolve] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::TypeError { message, span, .. } => {
                write!(f, "error[type] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::EvalError { message, span } => {
                write!(f, "error[eval] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::SchemaError { message, span } => {
                write!(f, "error[schema] at {}:{} — {}", span.line, span.col, message)
            }
        }
    }
}

impl std::error::Error for SparError {}
