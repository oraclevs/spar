use crate::ast::*;
use crate::error::{SparError, Span};
use crate::token::{SpannedToken, Token};

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).map(|st| &st.token).unwrap_or(&Token::Eof)
    }

    fn peek_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|st| st.span.clone())
            .unwrap_or_else(Span::dummy)
    }

    fn advance(&mut self) -> &SpannedToken {
        let st = &self.tokens[self.pos];
        self.pos += 1;
        st
    }

    fn expect(&mut self, expected: &Token) -> Result<SpannedToken, SparError> {
        if self.peek() == expected {
            Ok(self.advance().clone())
        } else {
            Err(SparError::ParseError {
                message: format!("expected {}, found {}", expected.human_name(), self.peek().human_name()),
                span: self.peek_span(),
            })
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span), SparError> {
        match self.peek() {
            Token::Ident(_) => {
                let st = self.advance().clone();
                if let Token::Ident(s) = st.token {
                    Ok((s, st.span))
                } else {
                    unreachable!()
                }
            }
            _ => Err(SparError::ParseError {
                message: format!("expected a name, found {}", self.peek().human_name()),
                span: self.peek_span(),
            }),
        }
    }

    fn at(&self, tok: &Token) -> bool {
        self.peek() == tok
    }

    #[allow(dead_code)]
    fn at_ident(&self) -> bool {
        matches!(self.peek(), Token::Ident(_))
    }

    fn error(&self, msg: impl Into<String>) -> SparError {
        SparError::ParseError {
            message: msg.into(),
            span: self.peek_span(),
        }
    }

    pub fn parse(mut self) -> Result<Program, SparError> {
        let is_schema_file = if self.at(&Token::At) {
            self.advance(); // consume '@'
            let (name, name_span) = self.expect_ident()?;
            if name != "SchemaFile" {
                return Err(SparError::ParseError {
                    message: format!("unknown file pragma `@{}`; only `@SchemaFile` is supported", name),
                    span: name_span,
                });
            }
            true
        } else {
            false
        };

        let mut items = Vec::new();
        loop {
            if self.at(&Token::Eof) { break; }
            items.push(self.parse_top_level_item()?);
        }

        // Validate schema-file exclusivity rules
        if is_schema_file {
            for item in &items {
                match item {
                    TopLevelItem::SchemaSection(_) => {}
                    _ => {
                        let item_span = match item {
                            TopLevelItem::Import(d) => d.span.clone(),
                            TopLevelItem::Var(d) => d.span.clone(),
                            TopLevelItem::Dynamic(d) => d.span.clone(),
                            TopLevelItem::Section(d) => d.span.clone(),
                            TopLevelItem::Function(d) => d.span.clone(),
                            TopLevelItem::SchemaSection(_) => unreachable!(),
                        };
                        return Err(SparError::ParseError {
                            message: "schema files may only contain `<Schema>` section declarations".to_string(),
                            span: item_span,
                        });
                    }
                }
            }
        } else {
            // Non-schema files must not have schema sections
            for item in &items {
                if let TopLevelItem::SchemaSection(s) = item {
                    return Err(SparError::ParseError {
                        message: format!(
                            "section `{}` has a `<Schema>` marker but this file is not a schema file — \
                             add `@SchemaFile` at the top of this file if it is intended to declare schema shapes",
                            s.name
                        ),
                        span: s.span.clone(),
                    });
                }
            }
        }

        Ok(Program { is_schema_file, items })
    }

    fn parse_top_level_item(&mut self) -> Result<TopLevelItem, SparError> {
        match self.peek() {
            Token::Import     => Ok(TopLevelItem::Import(self.parse_import()?)),
            Token::Var        => Ok(TopLevelItem::Var(self.parse_var_decl(false)?)),
            Token::Dynamic    => Ok(TopLevelItem::Dynamic(self.parse_dynamic_decl()?)),
            Token::LBracket   => self.parse_section_or_schema(false, false),
            Token::KwFunction => Ok(TopLevelItem::Function(self.parse_function_decl(false)?)),
            Token::Export => {
                self.advance();
                match self.peek() {
                    Token::Var      => Ok(TopLevelItem::Var(self.parse_var_decl(true)?)),
                    Token::LBracket => self.parse_section_or_schema(true, false),
                    _ => Err(self.error(format!("expected 'var' or '[' after 'export', found {}", self.peek().human_name()))),
                }
            }
            Token::Private => {
                self.advance(); // consume 'private'

                // Reject 'private var', 'private export', 'private dynamic'
                match self.peek() {
                    Token::Var | Token::Export | Token::Dynamic => {
                        return Err(self.error(
                            "'private' cannot be used with variables — \
                             use 'var' for private variables (they are not emitted by default) \
                             or 'export var' to include them in output".to_string()
                        ));
                    }
                    Token::LBracket => {
                        let item = self.parse_section_or_schema(false, true)?;
                        Ok(item)
                    }
                    Token::KwFunction => {
                        Ok(TopLevelItem::Function(self.parse_function_decl(true)?))
                    }
                    _ => {
                        Err(self.error(format!(
                            "'private' must be followed by 'function' or a section declaration '[SectionName]{{...}}', found {}",
                            self.peek().human_name()
                        )))
                    }
                }
            }
            Token::At => Err(self.error(
                "'@SchemaFile' pragma must be the first item in the file; \
                 it cannot appear mid-file"
            )),
            _ => Err(self.error(format!(
                "unexpected {}: expected 'import', 'var', 'export', 'dynamic', 'private', 'function', or '[' to start a declaration",
                self.peek().human_name()
            ))),
        }
    }

    fn parse_import(&mut self) -> Result<ImportDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::Import)?;

        // Detect contextual `schema` keyword: `import schema "path";`
        let is_schema = matches!(self.peek(), Token::Ident(s) if s == "schema");
        if is_schema {
            self.advance(); // consume 'schema' ident
            let path = self.parse_import_path()?;
            self.expect(&Token::Semicolon)?;
            return Ok(ImportDecl { path, alias: None, is_schema: true, span });
        }

        let path = self.parse_import_path()?;
        let alias = if self.at(&Token::As) {
            self.advance();
            let (name, _) = self.expect_ident()?;
            Some(name)
        } else {
            None
        };
        self.expect(&Token::Semicolon)?;
        Ok(ImportDecl { path, alias, is_schema: false, span })
    }

    fn parse_import_path(&mut self) -> Result<String, SparError> {
        self.expect(&Token::StringStart)?;

        let content = match self.peek() {
            Token::StringFragment(_) => {
                let st = self.advance().clone();
                if let Token::StringFragment(s) = st.token { s } else { unreachable!() }
            }
            Token::InterpolStart => {
                return Err(self.error("import paths cannot contain interpolation"));
            }
            Token::StringEnd => String::new(),
            _ => return Err(self.error(format!("expected string content, found {}", self.peek().human_name()))),
        };

        if self.at(&Token::InterpolStart) {
            return Err(self.error("import paths cannot contain interpolation"));
        }

        self.expect(&Token::StringEnd)?;
        Ok(content)
    }

    fn parse_var_decl(&mut self, exported: bool) -> Result<VarDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::Var)?;

        let (name, _) = self.expect_ident()?;

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;

        let value = if self.at(&Token::Eq) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };

        self.expect(&Token::Semicolon)?;
        Ok(VarDecl { exported, name, optional, ty, value, span })
    }

    fn parse_dynamic_decl(&mut self) -> Result<DynamicDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume `dynamic`
        self.expect(&Token::Var)?;

        let (name, _) = self.expect_ident()?;

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        let value = if self.at(&Token::Eq) {
            self.advance();
            if !self.at(&Token::LBracket) {
                return Err(self.error("dynamic variables must be assigned a list literal `[...]`"));
            }
            Some(self.parse_list_literal()?)
        } else {
            None
        };

        self.expect(&Token::Semicolon)?;
        Ok(DynamicDecl { name, optional, value, span })
    }

    fn parse_section_item(&mut self) -> Result<SectionItem, SparError> {
        match self.peek() {
            Token::DotDotDot => Ok(SectionItem::Spread(self.parse_spread()?)),
            Token::Ident(_)  => Ok(SectionItem::Field(self.parse_field_decl()?)),
            _ => Err(self.error("expected a field declaration or `...` spread")),
        }
    }

    fn parse_field_decl(&mut self) -> Result<FieldDecl, SparError> {
        let span = self.peek_span();
        let (name, _) = self.expect_ident()?;

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;

        let value = if self.at(&Token::Eq) {
            self.advance();
            if ty == SparType::Section && self.at(&Token::LBrace) {
                // Parse inline section body: '{' field_decl* '}'
                self.expect(&Token::LBrace)?;
                let mut fields = Vec::new();
                while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                    fields.push(self.parse_field_decl()?);
                }
                self.expect(&Token::RBrace)?;
                Some(FieldValue::Nested(fields))
            } else {
                Some(FieldValue::Expr(self.parse_expr()?))
            }
        } else {
            None
        };

        self.expect(&Token::Semicolon)?;
        Ok(FieldDecl { name, optional, ty, value, span })
    }

    fn parse_spread(&mut self) -> Result<SpreadStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::DotDotDot)?;
        let expr = self.parse_expr()?;
        self.expect(&Token::Semicolon)?;
        Ok(SpreadStmt { expr, span })
    }

    /// Try to consume `<Schema>` or `<Schema?>`. Returns `Some(SchemaMarker)` if
    /// a schema marker is present, `None` otherwise (leaves the token stream unchanged).
    fn try_parse_schema_marker(&mut self) -> Result<Option<SchemaMarker>, SparError> {
        if !self.at(&Token::Lt) {
            return Ok(None);
        }
        self.advance(); // consume '<'
        let (name, name_span) = self.expect_ident()?;
        if name != "Schema" {
            return Err(SparError::ParseError {
                message: format!("expected `Schema` after `<`, found `{}`", name),
                span: name_span,
            });
        }
        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&Token::Gt)?;
        Ok(Some(SchemaMarker { optional }))
    }

    /// Parse a single schema field: `name: Type;` or `name?: Type;`
    /// For section-typed fields: `name: section = { ... };`
    fn parse_schema_field(&mut self) -> Result<SchemaField, SparError> {
        let span = self.peek_span();
        let (name, _) = self.expect_ident()?;

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        self.expect(&Token::Colon)?;

        let shape = if self.at(&Token::TypeSection) {
            self.advance(); // consume 'section'
            self.expect(&Token::Eq)?;
            self.expect(&Token::LBrace)?;
            let mut nested = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                nested.push(self.parse_schema_field()?);
            }
            self.expect(&Token::RBrace)?;
            SchemaFieldShape::Section(nested)
        } else {
            let ty = self.parse_type()?;
            SchemaFieldShape::Primitive(ty)
        };

        self.expect(&Token::Semicolon)?;
        Ok(SchemaField { name, optional, shape, span })
    }

    fn parse_section_or_schema(&mut self, exported: bool, private: bool) -> Result<TopLevelItem, SparError> {
        let span = self.peek_span();
        self.expect(&Token::LBracket)?;

        let (name, _) = self.expect_ident()?;

        // Reject dot-separated paths (same as before)
        if self.at(&Token::Dot) {
            return Err(SparError::ParseError {
                message: format!(
                    "section names cannot contain '.': use the 'section' field type \
                     to nest sections inside '[{}]{{ ... }};'",
                    name
                ),
                span: self.peek_span(),
            });
        }

        self.expect(&Token::RBracket)?;

        // Check for schema marker BEFORE the `{`
        let schema_marker = self.try_parse_schema_marker()?;

        if let Some(marker) = schema_marker {
            self.expect(&Token::LBrace)?;
            let mut fields = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                fields.push(self.parse_schema_field()?);
            }
            self.expect(&Token::RBrace)?;
            // Schema sections do NOT have a trailing semicolon
            Ok(TopLevelItem::SchemaSection(SchemaSectionDecl {
                name,
                marker,
                fields,
                span,
            }))
        } else {
            // Regular config section
            self.expect(&Token::LBrace)?;
            let mut items = Vec::new();
            loop {
                match self.peek() {
                    Token::RBrace => break,
                    Token::Eof => return Err(self.error("unclosed section body — expected '}'")),
                    _ => items.push(self.parse_section_item()?),
                }
            }
            self.expect(&Token::RBrace)?;
            self.expect(&Token::Semicolon)?;
            Ok(TopLevelItem::Section(SectionDecl {
                exported,
                private,
                path: vec![name],
                items,
                span,
            }))
        }
    }

    fn parse_type(&mut self) -> Result<SparType, SparError> {
        if self.at(&Token::LBracket) {
            self.advance();
            let inner = self.parse_type()?;
            if inner == SparType::Section {
                return Err(self.error("'[section]' is not a valid type — 'section' cannot be used as a list element type"));
            }
            self.expect(&Token::RBracket)?;
            return Ok(SparType::List(Box::new(inner)));
        }
        self.parse_scalar_type()
    }

    fn parse_scalar_type(&mut self) -> Result<SparType, SparError> {
        let ty = match self.peek() {
            Token::TypeStr     => SparType::Str,
            Token::TypeInt     => SparType::Int,
            Token::TypeFloat   => SparType::Float,
            Token::TypeBool    => SparType::Bool,
            Token::TypeSection => SparType::Section,
            _ => return Err(self.error(format!("expected a type ('str', 'int', 'float', 'bool', or 'section'), found {}", self.peek().human_name()))),
        };
        self.advance();
        Ok(ty)
    }

    fn parse_expr(&mut self) -> Result<Expr, SparError> {
        self.parse_fallback_expr()
    }

    fn parse_fallback_expr(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let lhs = self.parse_or()?;
        if self.at(&Token::QuestionQuestion) {
            self.advance();
            let rhs = self.parse_fallback_expr()?;
            return Ok(Expr::BinaryOp(BinaryOp {
                op: BinOp::Fallback,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            }));
        }
        Ok(lhs)
    }

    fn parse_or(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let mut lhs = self.parse_and()?;
        while self.at(&Token::OrOr) {
            self.advance();
            let rhs = self.parse_and()?;
            lhs = Expr::BinaryOp(BinaryOp {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: span.clone(),
            });
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let mut lhs = self.parse_comparison()?;
        while self.at(&Token::AndAnd) {
            self.advance();
            let rhs = self.parse_comparison()?;
            lhs = Expr::BinaryOp(BinaryOp {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: span.clone(),
            });
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let mut lhs = self.parse_additive_expr()?;
        loop {
            let op = match self.peek() {
                Token::EqEq  => BinOp::Eq,
                Token::NotEq => BinOp::NotEq,
                Token::Lt    => BinOp::Lt,
                Token::Gt    => BinOp::Gt,
                Token::LtEq  => BinOp::LtEq,
                Token::GtEq  => BinOp::GtEq,
                _            => break,
            };
            self.advance();
            let rhs = self.parse_additive_expr()?;
            lhs = Expr::BinaryOp(BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: span.clone(),
            });
        }
        Ok(lhs)
    }

    fn parse_additive_expr(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let mut lhs = self.parse_mult_expr()?;
        loop {
            let op = match self.peek() {
                Token::Plus  => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _            => break,
            };
            self.advance();
            let rhs = self.parse_mult_expr()?;
            lhs = Expr::BinaryOp(BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: span.clone(),
            });
        }
        Ok(lhs)
    }

    fn parse_mult_expr(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Token::Star  => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _            => break,
            };
            self.advance();
            let rhs = self.parse_unary()?;
            lhs = Expr::BinaryOp(BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: span.clone(),
            });
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, SparError> {
        if self.at(&Token::Bang) {
            let span = self.peek_span();
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnOp::Not,
                operand: Box::new(operand),
                span,
            });
        }
        if self.at(&Token::Minus) {
            let span = self.peek_span();
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnOp::Neg,
                operand: Box::new(operand),
                span,
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, SparError> {
        let mut expr = match self.peek() {
            Token::IntLit(_) => {
                let st = self.advance().clone();
                if let Token::IntLit(n) = st.token {
                    Ok(Expr::Literal(Literal::Int(n)))
                } else {
                    unreachable!()
                }
            }
            Token::FloatLit(_) => {
                let st = self.advance().clone();
                if let Token::FloatLit(f) = st.token {
                    Ok(Expr::Literal(Literal::Float(f)))
                } else {
                    unreachable!()
                }
            }
            Token::True => {
                self.advance();
                Ok(Expr::Literal(Literal::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Literal(Literal::Bool(false)))
            }
            Token::StringStart => {
                let s = self.parse_interp_string()?;
                Ok(Expr::String(s))
            }
            Token::LBracket => self.parse_list_literal(),
            Token::LParen => {
                let span = self.peek_span();
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(Expr::Grouped(Box::new(inner), span))
            }
            Token::Ident(_) => self.parse_namespace_ref_or_fn_call(),
            Token::TypeStr => {
                let span = self.peek_span();
                self.advance();
                self.parse_fn_call("str".to_string(), span)
            }
            Token::TypeInt => {
                let span = self.peek_span();
                self.advance();
                self.parse_fn_call("int".to_string(), span)
            }
            Token::TypeFloat => {
                let span = self.peek_span();
                self.advance();
                self.parse_fn_call("float".to_string(), span)
            }
            Token::TypeBool => {
                let span = self.peek_span();
                self.advance();
                self.parse_fn_call("bool".to_string(), span)
            }
            Token::KwFor => self.parse_comprehension(),
            _ => Err(self.error(format!("expected an expression, found {}", self.peek().human_name()))),
        }?;

        // Postfix indexing: expr[index]
        while self.at(&Token::LBracket) {
            let span = self.peek_span();
            self.advance();
            let index = self.parse_expr()?;
            self.expect(&Token::RBracket)?;
            expr = Expr::Index {
                source: Box::new(expr),
                index: Box::new(index),
                span,
            };
        }

        Ok(expr)
    }

    fn parse_list_literal(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        self.expect(&Token::LBracket)?;

        let mut items = Vec::new();
        if !self.at(&Token::RBracket) {
            items.push(self.parse_expr()?);
            while self.at(&Token::Comma) {
                self.advance();
                if self.at(&Token::RBracket) {
                    break;
                }
                items.push(self.parse_expr()?);
            }
        }

        self.expect(&Token::RBracket)?;
        Ok(Expr::List(items, span))
    }

    fn parse_interp_string(&mut self) -> Result<InterpolString, SparError> {
        let span = self.peek_span();
        self.expect(&Token::StringStart)?;

        let mut parts = Vec::new();

        loop {
            match self.peek() {
                Token::StringFragment(_) => {
                    let st = self.advance().clone();
                    if let Token::StringFragment(s) = st.token {
                        parts.push(StringPart::Literal(s));
                    }
                }
                Token::StringEnd => {
                    self.advance();
                    break;
                }
                Token::InterpolStart => {
                    self.advance();
                    let expr = self.parse_expr()?;
                    self.expect(&Token::InterpolEnd)?;
                    parts.push(StringPart::Expr(Box::new(expr)));
                }
                _ => return Err(self.error(format!("unexpected {} inside string", self.peek().human_name()))),
            }
        }

        Ok(InterpolString { parts, span })
    }

    fn parse_namespace_ref_or_fn_call(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        let (name, name_span) = self.expect_ident()?;

        if self.at(&Token::LParen) {
            if name == "env" || name == "str" {
                return self.parse_fn_call(name, span);
            } else {
                return self.parse_user_call(name, name_span);
            }
        }

        let mut segments = vec![name];
        while self.at(&Token::ColonColon) {
            self.advance();
            let (seg, seg_span) = self.expect_ident()?;
            if self.at(&Token::LParen) {
                // cross-file call: alias::fn(args)
                let qualified = format!("{}::{}", segments.join("::"), seg);
                return self.parse_user_call(qualified, seg_span);
            }
            segments.push(seg);
        }

        Ok(Expr::NamespaceRef(NamespaceRef { segments, span }))
    }

    fn parse_fn_call(&mut self, name: String, span: Span) -> Result<Expr, SparError> {
        self.expect(&Token::LParen)?;

        let mut args = Vec::new();
        if !self.at(&Token::RParen) {
            args.push(self.parse_expr()?);
            while self.at(&Token::Comma) {
                self.advance();
                if self.at(&Token::RParen) {
                    break;
                }
                args.push(self.parse_expr()?);
            }
        }

        self.expect(&Token::RParen)?;
        Ok(Expr::FnCall(FnCall { name, args, span }))
    }

    fn parse_user_call(&mut self, name: String, name_span: Span) -> Result<Expr, SparError> {
        let span = name_span.clone();
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            let param_name_span = self.peek_span();
            let (param_name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let value = self.parse_or()?;
            args.push(CallArg {
                param_name,
                param_name_span: param_name_span.clone(),
                value,
                span: param_name_span,
            });
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        self.expect(&Token::RParen)?;
        Ok(Expr::Call { name, name_span, args, span })
    }

    fn parse_comprehension(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwFor)?;
        let (var_name, var_name_span) = self.expect_ident()?;
        self.expect(&Token::KwIn)?;
        let source = self.parse_or()?;
        self.expect(&Token::LBrace)?;
        let body = self.parse_or()?;
        self.expect(&Token::RBrace)?;
        Ok(Expr::Comprehension {
            var_name,
            var_name_span,
            source: Box::new(source),
            body: Box::new(body),
            span,
        })
    }

    fn parse_function_decl(&mut self, is_private: bool) -> Result<FunctionDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwFunction)?;
        let (name, name_span) = self.expect_ident()?;
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            let param_span = self.peek_span();
            let (param_name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param { name: param_name, ty, span: param_span });
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        self.expect(&Token::RParen)?;
        self.expect(&Token::Arrow)?;
        let ret_span = self.peek_span();
        let ret = self.parse_type()?;
        self.expect(&Token::LBrace)?;
        let mut stmts = Vec::new();
        while self.at(&Token::Var) || self.at(&Token::KwIf) || self.at(&Token::KwReturn) || self.at(&Token::KwFor) {
            stmts.push(self.parse_func_stmt()?);
        }
        let body_span = self.peek_span();
        self.expect(&Token::RBrace)?;
        Ok(FunctionDecl {
            name, name_span, params, ret, ret_span,
            body: FunctionBody { stmts, span: body_span },
            is_private,
            span,
        })
    }

    fn parse_func_stmt(&mut self) -> Result<FuncStmt, SparError> {
        if self.at(&Token::KwIf) {
            return Ok(FuncStmt::If(self.parse_if_stmt()?));
        }
        if self.at(&Token::KwFor) {
            return Ok(self.parse_for_stmt()?);
        }
        if self.at(&Token::KwReturn) {
            let start_span = self.peek_span();
            self.advance(); // consume 'return'
            let ret_value = if self.at(&Token::LBrace) {
                self.advance(); // consume '{'
                let mut fields = Vec::new();
                while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                    let field_span = self.peek_span();
                    let (field_name, _) = self.expect_ident()?;
                    self.expect(&Token::Colon)?;
                    let ty = self.parse_type()?;
                    self.expect(&Token::Eq)?;
                    let value = self.parse_or()?;
                    self.expect(&Token::Semicolon)?;
                    fields.push(ReturnField { name: field_name, ty, value, span: field_span });
                }
                self.expect(&Token::RBrace)?;
                ReturnValue::SectionBlock(fields)
            } else {
                ReturnValue::Expr(self.parse_or()?)
            };
            self.expect(&Token::Semicolon)?;
            return Ok(FuncStmt::Return(ret_value, start_span));
        }
        Ok(FuncStmt::LocalVar(self.parse_local_var_decl()?))
    }

    fn parse_for_stmt(&mut self) -> Result<FuncStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwFor)?;
        let (var_name, _) = self.expect_ident()?;
        self.expect(&Token::KwIn)?;
        let iterable = self.parse_or()?;
        self.expect(&Token::LBrace)?;
        let mut body = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            body.push(self.parse_func_stmt()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(FuncStmt::For { var_name, iterable, body, span })
    }

    fn parse_local_var_decl(&mut self) -> Result<LocalVarDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::Var)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&Token::Colon)?;
        let ty = self.parse_type()?;
        self.expect(&Token::Eq)?;
        let value = self.parse_or()?;
        self.expect(&Token::Semicolon)?;
        Ok(LocalVarDecl { name, ty, value, span })
    }

    fn parse_if_stmt(&mut self) -> Result<IfStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwIf)?;
        let condition = self.parse_or()?;
        self.expect(&Token::LBrace)?;
        let mut then_stmts = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            then_stmts.push(self.parse_func_stmt()?);
        }
        self.expect(&Token::RBrace)?;
        let else_stmts = if self.at(&Token::KwElse) {
            self.advance();
            self.expect(&Token::LBrace)?;
            let mut stmts = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                stmts.push(self.parse_func_stmt()?);
            }
            self.expect(&Token::RBrace)?;
            stmts
        } else {
            Vec::new()
        };
        Ok(IfStmt { condition, then_stmts, else_stmts, span })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        BinOp, Expr, FnCall, FieldValue, SparType, Literal, SectionItem, StringPart, TopLevelItem,
    };

    fn parse_str(src: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex failed");
        Parser::new(tokens).parse().expect("parse failed")
    }

    fn parse_err(src: &str) -> String {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex failed");
        Parser::new(tokens).parse().unwrap_err().to_string()
    }

    fn first_item(src: &str) -> TopLevelItem {
        parse_str(src).items.into_iter().next().expect("no items")
    }

    #[test]
    fn test_import_no_alias() {
        let item = first_item(r#"import "base.spar";"#);
        let TopLevelItem::Import(decl) = item else { panic!("not import") };
        assert_eq!(decl.path, "base.spar");
        assert_eq!(decl.alias, None);
    }

    #[test]
    fn test_import_with_alias() {
        let item = first_item(r#"import "config/base.spar" as config;"#);
        let TopLevelItem::Import(decl) = item else { panic!("not import") };
        assert_eq!(decl.path, "config/base.spar");
        assert_eq!(decl.alias, Some("config".into()));
    }

    #[test]
    fn test_import_interpolation_error() {
        let err = parse_err(r#"import "${bad}.spar";"#);
        assert!(err.contains("import paths cannot contain interpolation"), "got: {err}");
    }

    #[test]
    fn test_required_var_decl() {
        let item = first_item("var port: int = 3000;");
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        assert!(!decl.exported);
        assert_eq!(decl.name, "port");
        assert!(!decl.optional);
        assert_eq!(decl.ty, SparType::Int);
        assert!(decl.value.is_some());
        let val = decl.value.unwrap();
        assert!(matches!(val, Expr::Literal(Literal::Int(3000))));
    }

    #[test]
    fn test_optional_var_no_value() {
        let item = first_item("var log_level?: str;");
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        assert!(decl.optional);
        assert_eq!(decl.ty, SparType::Str);
        assert!(decl.value.is_none());
    }

    #[test]
    fn test_exported_var() {
        let item = first_item(r#"export var version: str = "1.0.0";"#);
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        assert!(decl.exported);
        assert_eq!(decl.name, "version");
    }

    #[test]
    fn test_typed_list_var() {
        let item = first_item("var ports: [int] = [3000, 8080];");
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        assert_eq!(decl.ty, SparType::List(Box::new(SparType::Int)));
        assert!(matches!(decl.value, Some(Expr::List(_, _))));
    }

    #[test]
    fn test_dynamic_var_with_value() {
        let item = first_item(r#"dynamic var tags = [2026, "prod", true];"#);
        let TopLevelItem::Dynamic(decl) = item else { panic!("not dynamic") };
        assert_eq!(decl.name, "tags");
        assert!(!decl.optional);
        assert!(matches!(decl.value, Some(Expr::List(_, _))));
    }

    #[test]
    fn test_dynamic_var_optional_no_value() {
        let item = first_item("dynamic var meta?;");
        let TopLevelItem::Dynamic(decl) = item else { panic!("not dynamic") };
        assert!(decl.optional);
        assert!(decl.value.is_none());
    }

    #[test]
    fn test_dynamic_non_list_error() {
        let err = parse_err("dynamic var bad = 3000;");
        assert!(err.contains("dynamic variables must be assigned a list literal"), "got: {err}");
    }

    #[test]
    fn test_simple_section() {
        let item = first_item("[server]{ port: int = 3000; };");
        let TopLevelItem::Section(decl) = item else { panic!("not section") };
        assert!(!decl.exported);
        assert_eq!(decl.path, vec!["server"]);
        assert_eq!(decl.items.len(), 1);
        assert!(matches!(decl.items[0], SectionItem::Field(_)));
    }

    #[test]
    fn test_exported_section() {
        let item = first_item("export [defaults]{ workers: int = 4; };");
        let TopLevelItem::Section(decl) = item else { panic!("not section") };
        assert!(decl.exported);
    }

    #[test]
    fn dot_path_section_name_is_rejected() {
        let src = "[templates.CustomFolder]{ x: int = 1; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let result = Parser::new(tokens).parse();
        assert!(result.is_err(), "dot-path section names must be rejected");
        let err = result.unwrap_err();
        let msg = match &err {
            crate::error::SparError::ParseError { message, .. } => message.clone(),
            _ => panic!("expected ParseError"),
        };
        assert!(
            msg.contains("'.'") || msg.contains("section"),
            "error message should explain the dot restriction, got: {msg}"
        );
    }

    #[test]
    fn test_optional_field_in_section() {
        let item = first_item("[server]{ log_level?: str; };");
        let TopLevelItem::Section(decl) = item else { panic!("not section") };
        let SectionItem::Field(f) = &decl.items[0] else { panic!("not field") };
        assert!(f.optional);
        assert!(f.value.is_none());
    }

    #[test]
    fn test_local_spread() {
        let item = first_item("[project]{ ...base_project; };");
        let TopLevelItem::Section(decl) = item else { panic!("not section") };
        let SectionItem::Spread(s) = &decl.items[0] else { panic!("not spread") };
        let Expr::NamespaceRef(nr) = &s.expr else { panic!("expected NamespaceRef") };
        assert_eq!(nr.segments, vec!["base_project"]);
    }

    #[test]
    fn test_namespaced_spread() {
        let item = first_item("[project]{ ...global::base_project; };");
        let TopLevelItem::Section(decl) = item else { panic!("not section") };
        let SectionItem::Spread(s) = &decl.items[0] else { panic!("not spread") };
        let Expr::NamespaceRef(nr) = &s.expr else { panic!("expected NamespaceRef") };
        assert_eq!(nr.segments, vec!["global", "base_project"]);
    }

    #[test]
    fn test_arithmetic_expr() {
        let item = first_item("var timeout: int = 30 * 3;");
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        let Some(Expr::BinaryOp(op)) = decl.value else { panic!("not binop") };
        assert_eq!(op.op, BinOp::Mul);
        assert!(matches!(*op.lhs, Expr::Literal(Literal::Int(30))));
        assert!(matches!(*op.rhs, Expr::Literal(Literal::Int(3))));
    }

    #[test]
    fn test_fallback_expr() {
        let item = first_item(r#"var port: int = env("PORT") ?? 3000;"#);
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        let Some(Expr::BinaryOp(op)) = decl.value else { panic!("not binop") };
        assert_eq!(op.op, BinOp::Fallback);
        assert!(matches!(*op.lhs, Expr::FnCall(FnCall { ref name, .. }) if name == "env"));
        assert!(matches!(*op.rhs, Expr::Literal(Literal::Int(3000))));
    }

    #[test]
    fn test_namespace_ref() {
        let item = first_item("var x: int = global::port;");
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        let Some(Expr::NamespaceRef(nr)) = decl.value else { panic!("not ns ref") };
        assert_eq!(nr.segments, vec!["global", "port"]);
    }

    #[test]
    fn test_interpolated_string_expr() {
        let item = first_item(r#"var url: str = "http://${global::host}";"#);
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        let Some(Expr::String(s)) = decl.value else { panic!("not string") };
        assert_eq!(s.parts.len(), 3);
        assert!(matches!(&s.parts[0], StringPart::Literal(l) if l == "http://"));
        let StringPart::Expr(e) = &s.parts[1] else { panic!("not expr part") };
        let Expr::NamespaceRef(nr) = e.as_ref() else { panic!("not ns ref") };
        assert_eq!(nr.segments, vec!["global", "host"]);
        assert!(matches!(&s.parts[2], StringPart::Literal(l) if l.is_empty()));
    }

    #[test]
    fn test_env_fn_call() {
        let item = first_item(r#"var mode: str = env("APP_MODE");"#);
        let TopLevelItem::Var(decl) = item else { panic!("not var") };
        let Some(Expr::FnCall(fc)) = decl.value else { panic!("not fn call") };
        assert_eq!(fc.name, "env");
        assert_eq!(fc.args.len(), 1);
        assert!(matches!(fc.args[0], Expr::String(_)));
    }

    #[test]
    fn test_unknown_fn_error() {
        // User-defined function calls require named args; positional args cause a parse error
        let err = parse_err(r#"var x: str = foo("bar");"#);
        assert!(err.contains("expected a name"), "got: {err}");
    }

    #[test]
    fn test_unclosed_section_error() {
        let err = parse_err("[server]{");
        assert!(err.contains("unclosed section"), "got: {err}");
    }

    #[test]
    fn test_missing_semicolon_after_section() {
        let err = parse_err("[server]{ port: int = 3000; }");
        assert!(err.contains("expected"), "got: {err}");
    }

    #[test]
    fn test_full_program_multiple_decls() {
        let src = r#"
            import "base.spar";
            var port: int = 3000;
            [server]{ host: str = "localhost"; };
        "#;
        let prog = parse_str(src);
        assert_eq!(prog.items.len(), 3);
    }

    #[test]
    fn section_field_type_parses() {
        let src = r#"[MetaData]{ manual: section = { author: str = "occ"; }; };"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let result = Parser::new(tokens).parse();
        assert!(result.is_ok(), "section field should parse: {:?}", result.err());
    }

    #[test]
    fn nested_section_twice_deep_parses() {
        let src = "[A]{ b: section = { c: section = { val: int = 1; }; }; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok(), "two-deep nesting should parse");
    }

    #[test]
    fn empty_nested_section_parses() {
        let src = "[A]{ inner: section = { }; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok(), "empty nested section should parse");
    }

    #[test]
    fn list_section_type_rejected() {
        let src = "[A]{ x: [section]; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let result = Parser::new(tokens).parse();
        assert!(result.is_err(), "[section] should be rejected");
    }

    #[test]
    fn section_field_value_is_nested() {
        let src = r#"[A]{ inner: section = { key: str = "v"; }; };"#;
        let prog = parse_str(src);
        let TopLevelItem::Section(decl) = &prog.items[0] else { panic!() };
        let SectionItem::Field(f) = &decl.items[0] else { panic!() };
        assert!(matches!(f.value, Some(FieldValue::Nested(_))));
    }

    #[test]
    fn private_section_parses() {
        let src = "private [Defaults]{ timeout: int = 30; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn return_inside_bare_if_parses() {
        let src = r#"
function f(score: int) -> str {
    if score >= 90 { return "A"; }
    return "C";
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn return_inside_both_if_and_else_parses() {
        let src = r#"
function f(debug: bool) -> int {
    if debug { return 1; } else { return 2; }
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn return_with_section_block_inside_if_parses() {
        let src = r#"
function f(debug: bool) -> section {
    if debug {
        return { mode: str = str(true); };
    }
    return { mode: str = str(false); };
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn multiple_local_vars_then_return_in_sequence_parses() {
        let src = r#"
function f(a: int) -> int {
    var x: int = a;
    var y: int = x;
    return y;
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn bare_if_without_else_parses() {
        let src = r#"
function f(flag: bool) -> int {
    if flag { var x: int = 1; }
    return 0;
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn private_var_is_rejected() {
        let src = "private var x: int = 1;";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let result = Parser::new(tokens).parse();
        assert!(result.is_err(), "'private var' must be a parse error");
        let e = result.unwrap_err();
        let msg = format!("{e:?}");
        assert!(
            msg.contains("variable") || msg.contains("export var"),
            "error message must explain 'private var' is invalid, got: {msg}"
        );
    }
}
