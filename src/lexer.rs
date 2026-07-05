use crate::error::{SparError, Span};
use crate::token::{keyword_or_ident, SpannedToken, Token};

#[derive(Debug, Clone)]
pub struct CommentTrivia {
    pub text: String,
    pub line: u32,
    pub is_trailing: bool,
}

pub struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    last_token_line: u32,
    comments: Vec<CommentTrivia>,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            last_token_line: 0,
            comments: Vec::new(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.bytes.get(self.pos).copied()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn span_at(&self, start: usize, start_line: u32, start_col: u32) -> Span {
        Span::new(start, self.pos, start_line, start_col)
    }

    fn skip_line_comment(&mut self) {
        while let Some(b) = self.peek() {
            if b == b'\n' { break; }
            self.advance();
        }
    }

    fn collect_line_comment(&mut self, text_start: usize, comment_line: u32) {
        self.skip_line_comment();
        let text = self.source[text_start..self.pos].trim_end().to_string();
        self.comments.push(CommentTrivia {
            text,
            line: comment_line,
            is_trailing: self.last_token_line == comment_line,
        });
    }

    fn skip_block_comment(&mut self, start: usize, line: u32, col: u32) -> Result<(), SparError> {
        loop {
            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated block comment".to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b'*') if self.peek_at(1) == Some(b'/') => {
                    self.advance();
                    self.advance();
                    return Ok(());
                }
                _ => { self.advance(); }
            }
        }
    }

    fn collect_block_comment(&mut self, text_start: usize, comment_line: u32, err_span_start: usize, err_line: u32, err_col: u32) -> Result<(), SparError> {
        let is_trailing = self.last_token_line == comment_line;
        self.skip_block_comment(err_span_start, err_line, err_col)?;
        let text = self.source[text_start..self.pos].to_string();
        self.comments.push(CommentTrivia { text, line: comment_line, is_trailing });
        Ok(())
    }

    fn lex_string(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        start: usize,
        start_line: u32,
        start_col: u32,
    ) -> Result<(), SparError> {
        tokens.push(SpannedToken::new(
            Token::StringStart,
            Span::new(start, start, start_line, start_col),
        ));

        let mut fragment = String::new();

        loop {
            let frag_start = self.pos;
            let frag_line = self.line;
            let frag_col = self.col;

            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated string".to_string(),
                        span: Span::new(frag_start, self.pos, frag_line, frag_col),
                    });
                }
                Some(b'\n') => {
                    return Err(SparError::LexError {
                        message: "unterminated string".to_string(),
                        span: Span::new(frag_start, self.pos, frag_line, frag_col),
                    });
                }
                Some(b'"') => {
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::StringFragment(fragment),
                        Span::new(frag_start, self.pos, frag_line, frag_col),
                    ));
                    tokens.push(SpannedToken::new(
                        Token::StringEnd,
                        Span::new(self.pos, self.pos, self.line, self.col),
                    ));
                    return Ok(());
                }
                Some(b'$') if self.peek_at(1) == Some(b'{') => {
                    // emit current fragment (even if empty)
                    tokens.push(SpannedToken::new(
                        Token::StringFragment(fragment.clone()),
                        Span::new(frag_start, self.pos, frag_line, frag_col),
                    ));
                    fragment.clear();

                    self.advance(); // $
                    self.advance(); // {

                    let interp_span = Span::new(self.pos - 2, self.pos, self.line, self.col);
                    tokens.push(SpannedToken::new(Token::InterpolStart, interp_span));

                    let mut brace_depth: u32 = 1;
                    self.tokenize_interp(tokens, &mut brace_depth)?;

                    tokens.push(SpannedToken::new(
                        Token::InterpolEnd,
                        Span::new(self.pos, self.pos, self.line, self.col),
                    ));
                }
                Some(b'\\') => {
                    self.advance(); // backslash
                    match self.peek() {
                        Some(b'\\') => { self.advance(); fragment.push('\\'); }
                        Some(b'"')  => { self.advance(); fragment.push('"'); }
                        Some(b'n')  => { self.advance(); fragment.push('\n'); }
                        Some(b't')  => { self.advance(); fragment.push('\t'); }
                        Some(c) => {
                            self.advance();
                            fragment.push('\\');
                            fragment.push(c as char);
                        }
                        None => {
                            return Err(SparError::LexError {
                                message: "unterminated string".to_string(),
                                span: Span::new(frag_start, self.pos, frag_line, frag_col),
                            });
                        }
                    }
                }
                Some(c) => {
                    self.advance();
                    fragment.push(c as char);
                }
            }
        }
    }

    fn tokenize_interp(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        brace_depth: &mut u32,
    ) -> Result<(), SparError> {
        loop {
            let start = self.pos;
            let line = self.line;
            let col = self.col;

            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated string".to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => {
                    self.advance();
                }
                Some(b'"') => {
                    return Err(SparError::LexError {
                        message: "nested strings inside interpolation are not supported".to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b'{') => {
                    self.advance();
                    *brace_depth += 1;
                    tokens.push(SpannedToken::new(Token::LBrace, self.span_at(start, line, col)));
                }
                Some(b'}') => {
                    self.advance();
                    *brace_depth -= 1;
                    if *brace_depth == 0 {
                        return Ok(());
                    }
                    tokens.push(SpannedToken::new(Token::RBrace, self.span_at(start, line, col)));
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    self.advance();
                    self.advance();
                    self.collect_line_comment(start, line);
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.advance();
                    self.advance();
                    self.collect_block_comment(start, line, start, line, col)?;
                }
                Some(c) => {
                    let tok = self.lex_single_token(c, start, line, col)?;
                    if let Some(t) = tok {
                        tokens.push(t);
                    }
                }
            }
        }
    }

    fn lex_single_token(
        &mut self,
        c: u8,
        start: usize,
        line: u32,
        col: u32,
    ) -> Result<Option<SpannedToken>, SparError> {
        let tok = match c {
            b'+' => { self.advance(); Token::Plus }
            b'-' => {
                if self.peek_at(1) == Some(b'>') {
                    self.advance(); self.advance();
                    Token::Arrow
                } else {
                    self.advance();
                    Token::Minus
                }
            }
            b'*' => { self.advance(); Token::Star }
            b'=' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance(); self.advance();
                    Token::EqEq
                } else {
                    self.advance();
                    Token::Eq
                }
            }
            b';' => { self.advance(); Token::Semicolon }
            b',' => { self.advance(); Token::Comma }
            b'(' => { self.advance(); Token::LParen }
            b')' => { self.advance(); Token::RParen }
            b'[' => { self.advance(); Token::LBracket }
            b']' => { self.advance(); Token::RBracket }

            b'?' => {
                self.advance();
                if self.peek() == Some(b'?') {
                    self.advance();
                    Token::QuestionQuestion
                } else {
                    Token::Question
                }
            }
            b':' => {
                self.advance();
                if self.peek() == Some(b':') {
                    self.advance();
                    Token::ColonColon
                } else {
                    Token::Colon
                }
            }
            b'.' => {
                self.advance();
                if self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.') {
                    self.advance();
                    self.advance();
                    Token::DotDotDot
                } else {
                    Token::Dot
                }
            }
            b'/' => {
                self.advance();
                if self.peek() == Some(b'/') {
                    self.advance();
                    self.collect_line_comment(start, line);
                    return Ok(None);
                } else if self.peek() == Some(b'*') {
                    self.advance();
                    self.collect_block_comment(start, line, start, line, col)?;
                    return Ok(None);
                } else {
                    Token::Slash
                }
            }

            b'!' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance(); self.advance();
                    Token::NotEq
                } else {
                    self.advance();
                    Token::Bang
                }
            }
            b'<' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance(); self.advance();
                    Token::LtEq
                } else {
                    self.advance();
                    Token::Lt
                }
            }
            b'>' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance(); self.advance();
                    Token::GtEq
                } else {
                    self.advance();
                    Token::Gt
                }
            }
            b'&' => {
                if self.peek_at(1) == Some(b'&') {
                    self.advance(); self.advance();
                    Token::AndAnd
                } else {
                    return Err(SparError::LexError {
                        message: "unexpected '&' — did you mean '&&'?".into(),
                        span: self.span_at(start, line, col),
                    });
                }
            }
            b'|' => {
                if self.peek_at(1) == Some(b'|') {
                    self.advance(); self.advance();
                    Token::OrOr
                } else {
                    return Err(SparError::LexError {
                        message: "unexpected '|' — did you mean '||'?".into(),
                        span: self.span_at(start, line, col),
                    });
                }
            }

            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let s = self.read_ident();
                keyword_or_ident(s)
            }

            b'0'..=b'9' => {
                self.read_number(start, line, col)?
            }

            b'@' => { self.advance(); Token::At }

            other => {
                self.advance();
                return Err(SparError::LexError {
                    message: format!("unexpected character '{}'", other as char),
                    span: Span::new(start, self.pos, line, col),
                });
            }
        };

        Ok(Some(SpannedToken::new(tok, self.span_at(start, line, col))))
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.advance();
            } else {
                break;
            }
        }
        self.source[start..self.pos].to_string()
    }

    fn read_number(&mut self, start: usize, line: u32, col: u32) -> Result<Token, SparError> {
        let num_start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }

        // check for float: digit '.' digit
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.advance(); // .
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
            let s = &self.source[num_start..self.pos];
            s.parse::<f64>().map(Token::FloatLit).map_err(|_| SparError::LexError {
                message: "invalid number literal".to_string(),
                span: Span::new(start, self.pos, line, col),
            })
        } else {
            let s = &self.source[num_start..self.pos];
            s.parse::<i64>().map(Token::IntLit).map_err(|_| SparError::LexError {
                message: "invalid number literal".to_string(),
                span: Span::new(start, self.pos, line, col),
            })
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<SpannedToken>, SparError> {
        self.tokenize_inner()
    }

    pub fn tokenize_with_comments(mut self) -> Result<(Vec<SpannedToken>, Vec<CommentTrivia>), SparError> {
        let tokens = self.tokenize_inner()?;
        Ok((tokens, self.comments))
    }

    fn tokenize_inner(&mut self) -> Result<Vec<SpannedToken>, SparError> {
        let mut tokens: Vec<SpannedToken> = Vec::new();

        loop {
            let start = self.pos;
            let line = self.line;
            let col = self.col;

            let c = match self.peek() {
                None => break,
                Some(c) => c,
            };

            match c {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    self.advance();
                }
                b'"' => {
                    self.advance();
                    self.lex_string(&mut tokens, start, line, col)?;
                    self.last_token_line = line;
                }
                b'{' => {
                    self.advance();
                    tokens.push(SpannedToken::new(Token::LBrace, self.span_at(start, line, col)));
                    self.last_token_line = line;
                }
                b'}' => {
                    self.advance();
                    tokens.push(SpannedToken::new(Token::RBrace, self.span_at(start, line, col)));
                    self.last_token_line = line;
                }
                _ => {
                    if let Some(t) = self.lex_single_token(c, start, line, col)? {
                        self.last_token_line = line;
                        tokens.push(t);
                    }
                }
            }
        }

        tokens.push(SpannedToken::new(
            Token::Eof,
            Span::new(self.pos, self.pos, self.line, self.col),
        ));

        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    fn lex(src: &str) -> Vec<Token> {
        Lexer::new(src)
            .tokenize()
            .expect("lex failed")
            .into_iter()
            .map(|st| st.token)
            .collect()
    }

    #[test]
    fn test_basic_var_decl() {
        assert_eq!(
            lex("var port: int = 3000;"),
            vec![
                Token::Var,
                Token::Ident("port".into()),
                Token::Colon,
                Token::TypeInt,
                Token::Eq,
                Token::IntLit(3000),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_export_var() {
        let tokens = lex(r#"export var version: str = "1.0.0";"#);
        assert_eq!(tokens[0], Token::Export);
        assert_eq!(tokens[1], Token::Var);
        assert_eq!(tokens[2], Token::Ident("version".into()));
        assert_eq!(tokens[3], Token::Colon);
        assert_eq!(tokens[4], Token::TypeStr);
        assert_eq!(tokens[5], Token::Eq);
        assert_eq!(tokens[6], Token::StringStart);
    }

    #[test]
    fn test_plain_string() {
        assert_eq!(
            lex(r#""base.cl""#),
            vec![
                Token::StringStart,
                Token::StringFragment("base.cl".into()),
                Token::StringEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_interp_single() {
        assert_eq!(
            lex(r#""http://${host}""#),
            vec![
                Token::StringStart,
                Token::StringFragment("http://".into()),
                Token::InterpolStart,
                Token::Ident("host".into()),
                Token::InterpolEnd,
                Token::StringFragment("".into()),
                Token::StringEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_interp_multiple() {
        assert_eq!(
            lex(r#""${host}:${port}""#),
            vec![
                Token::StringStart,
                Token::StringFragment("".into()),
                Token::InterpolStart,
                Token::Ident("host".into()),
                Token::InterpolEnd,
                Token::StringFragment(":".into()),
                Token::InterpolStart,
                Token::Ident("port".into()),
                Token::InterpolEnd,
                Token::StringFragment("".into()),
                Token::StringEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_import_with_alias() {
        assert_eq!(
            lex(r#"import "base.cl" as config;"#),
            vec![
                Token::Import,
                Token::StringStart,
                Token::StringFragment("base.cl".into()),
                Token::StringEnd,
                Token::As,
                Token::Ident("config".into()),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_section_decl() {
        assert_eq!(
            lex("[server]{"),
            vec![
                Token::LBracket,
                Token::Ident("server".into()),
                Token::RBracket,
                Token::LBrace,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_multi_char_ops() {
        assert_eq!(
            lex("?? :: ..."),
            vec![
                Token::QuestionQuestion,
                Token::ColonColon,
                Token::DotDotDot,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_namespace_ref() {
        assert_eq!(
            lex("global::port"),
            vec![
                Token::Ident("global".into()),
                Token::ColonColon,
                Token::Ident("port".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_float_literal() {
        assert_eq!(lex("3.14"), vec![Token::FloatLit(3.14), Token::Eof]);
    }

    #[test]
    fn test_int_not_float() {
        assert_eq!(lex("3000"), vec![Token::IntLit(3000), Token::Eof]);
    }

    #[test]
    fn test_dynamic_decl() {
        let tokens = lex(r#"dynamic var tags = [2026, "prod", true];"#);
        assert_eq!(tokens[0], Token::Dynamic);
        assert_eq!(tokens[1], Token::Var);
        assert_eq!(tokens[2], Token::Ident("tags".into()));
    }

    #[test]
    fn test_bool_keywords() {
        assert_eq!(
            lex("true false"),
            vec![Token::True, Token::False, Token::Eof]
        );
    }

    #[test]
    fn test_all_keywords() {
        assert_eq!(
            lex("var export import as dynamic str int float bool true false section"),
            vec![
                Token::Var,
                Token::Export,
                Token::Import,
                Token::As,
                Token::Dynamic,
                Token::TypeStr,
                Token::TypeInt,
                Token::TypeFloat,
                Token::TypeBool,
                Token::True,
                Token::False,
                Token::TypeSection,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_section_keyword() {
        assert_eq!(
            lex("section"),
            vec![Token::TypeSection, Token::Eof]
        );
    }

    #[test]
    fn test_line_comment_skipped() {
        assert_eq!(
            lex("var // this is ignored\nport"),
            vec![Token::Var, Token::Ident("port".into()), Token::Eof]
        );
    }

    #[test]
    fn test_block_comment_skipped() {
        assert_eq!(
            lex("var /* ignored */ port"),
            vec![Token::Var, Token::Ident("port".into()), Token::Eof]
        );
    }

    #[test]
    fn test_line_comment_collected_as_trivia() {
        let (tokens, comments) = Lexer::new("var x: int = 1; // trailing\n// standalone\nvar y: int = 2;")
            .tokenize_with_comments().expect("lex failed");
        // tokens still work
        assert!(tokens.iter().any(|t| t.token == Token::Var));
        // two comments
        assert_eq!(comments.len(), 2, "got: {:?}", comments);
        assert!(comments[0].is_trailing, "first should be trailing");
        assert_eq!(comments[0].line, 1);
        assert!(!comments[1].is_trailing, "second should be standalone");
        assert_eq!(comments[1].line, 2);
        assert!(comments[1].text.contains("standalone"), "text: {:?}", comments[1].text);
    }

    #[test]
    fn test_unclosed_block_comment() {
        let result = Lexer::new("/* not closed").tokenize();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unterminated block comment"), "got: {msg}");
    }

    #[test]
    fn test_unterminated_string() {
        let result = Lexer::new("\"not closed").tokenize();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unterminated string"), "got: {msg}");
    }

    #[test]
    fn test_optional_field() {
        assert_eq!(
            lex("log_level?: str"),
            vec![
                Token::Ident("log_level".into()),
                Token::Question,
                Token::Colon,
                Token::TypeStr,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_arith_and_fallback() {
        assert_eq!(
            lex("30 * 3 ?? 90"),
            vec![
                Token::IntLit(30),
                Token::Star,
                Token::IntLit(3),
                Token::QuestionQuestion,
                Token::IntLit(90),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn lex_new_tokens() {
        let cases: &[(&str, Token)] = &[
            ("->",  Token::Arrow),
            ("==",  Token::EqEq),
            ("!=",  Token::NotEq),
            ("<=",  Token::LtEq),
            (">=",  Token::GtEq),
            ("&&",  Token::AndAnd),
            ("||",  Token::OrOr),
            ("!",   Token::Bang),
            ("<",   Token::Lt),
            (">",   Token::Gt),
        ];
        for (src, expected) in cases {
            let tokens = Lexer::new(src).tokenize().unwrap();
            assert_eq!(tokens[0].token, *expected, "failed on {src:?}");
        }
    }

    #[test]
    fn lex_keywords_function_return_if_else_for_in() {
        let cases: &[(&str, Token)] = &[
            ("function", Token::KwFunction),
            ("return",   Token::KwReturn),
            ("if",       Token::KwIf),
            ("else",     Token::KwElse),
            ("for",      Token::KwFor),
            ("in",       Token::KwIn),
        ];
        for (src, expected) in cases {
            let tokens = Lexer::new(src).tokenize().unwrap();
            assert_eq!(tokens[0].token, *expected, "failed on {src:?}");
        }
    }

    #[test]
    fn minus_not_arrow() {
        let tokens = Lexer::new("x - y").tokenize().unwrap();
        assert_eq!(tokens[1].token, Token::Minus);
    }

    #[test]
    fn eq_not_eqeq() {
        let tokens = Lexer::new("x = 1").tokenize().unwrap();
        assert_eq!(tokens[1].token, Token::Eq);
    }

    #[test]
    fn lex_at_sign() {
        let tokens = Lexer::new("@").tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::At);
    }

    #[test]
    fn lex_at_before_ident() {
        let tokens = Lexer::new("@SchemaFile").tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::At);
        assert_eq!(tokens[1].token, Token::Ident("SchemaFile".to_string()));
    }
}
