use crate::ast::*;
use crate::error::{Span, SparError};
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
        self.tokens
            .get(self.pos)
            .map(|st| &st.token)
            .unwrap_or(&Token::Eof)
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
                message: format!(
                    "expected {}, found {}",
                    expected.human_name(),
                    self.peek().human_name()
                ),
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
        let (is_schema_file, load_env) = if self.at(&Token::At) {
            self.advance(); // consume '@'
            let (name, name_span) = self.expect_ident()?;
            match name.as_str() {
                "SchemaFile" => (true, None),
                "LoadEnv" => {
                    let path = if self.at(&Token::LParen) {
                        self.advance();
                        let path = self.parse_load_env_path()?;
                        self.expect(&Token::RParen)?;
                        path
                    } else {
                        ".env".to_string()
                    };
                    (false, Some(path))
                }
                _ => {
                    return Err(SparError::ParseError {
                        message: format!(
                            "unknown file pragma `@{}`; only `@SchemaFile` or `@LoadEnv` is supported",
                            name
                        ),
                        span: name_span,
                    });
                }
            }
        } else {
            (false, None)
        };

        let mut items = Vec::new();
        loop {
            if self.at(&Token::Eof) {
                break;
            }
            items.push(self.parse_top_level_item()?);
        }

        // Validate schema-file exclusivity rules
        if is_schema_file {
            for item in &items {
                let allowed = matches!(item, TopLevelItem::SchemaSection(_))
                    || matches!(item, TopLevelItem::SchemaFrom(_))
                    || matches!(item, TopLevelItem::Import(d) if matches!(d.kind, ImportKind::TypeSelective(_)));
                if !allowed {
                    let item_span = match item {
                        TopLevelItem::Import(d) => d.span.clone(),
                        TopLevelItem::Var(d) => d.span.clone(),
                        TopLevelItem::Dynamic(d) => d.span.clone(),
                        TopLevelItem::Section(d) => d.span.clone(),
                        TopLevelItem::Function(d) => d.span.clone(),
                        TopLevelItem::Type(d) => d.span.clone(),
                        TopLevelItem::Enum(d) => d.span.clone(),
                        TopLevelItem::FunctionGroup(d) => d.span.clone(),
                        TopLevelItem::SchemaFrom(d) => d.span.clone(),
                        TopLevelItem::Task(d) => d.span.clone(),
                        TopLevelItem::SchemaSection(_) => unreachable!(),
                    };
                    return Err(SparError::ParseError {
                        message: "schema files may only contain `Schema [Name]{...}` declarations, \
                                   `import type {...} from \"...\";`, and `SchemaFrom [Name, Type];`".to_string(),
                        span: item_span,
                    });
                }
            }
        } else {
            // Non-schema files must not have schema sections or SchemaFrom
            for item in &items {
                if let TopLevelItem::SchemaSection(s) = item {
                    return Err(SparError::ParseError {
                        message: format!(
                            "`Schema [{}]{{...}};` declares a schema, but this file is not a schema file — \
                             add `@SchemaFile` at the top of this file if it is intended to declare schema shapes",
                            s.name
                        ),
                        span: s.span.clone(),
                    });
                }
                if let TopLevelItem::SchemaFrom(sf) = item {
                    return Err(SparError::ParseError {
                        message: "`SchemaFrom [...]` is only legal inside a schema file — \
                                   add `@SchemaFile` at the top of this file"
                            .to_string(),
                        span: sf.span.clone(),
                    });
                }
            }
        }

        Ok(Program {
            is_schema_file,
            load_env,
            items,
        })
    }

    fn parse_load_env_path(&mut self) -> Result<String, SparError> {
        self.expect(&Token::StringStart)?;

        let path = match self.peek() {
            Token::StringFragment(_) => {
                let st = self.advance().clone();
                let Token::StringFragment(path) = st.token else {
                    unreachable!()
                };
                path
            }
            Token::InterpolStart => {
                return Err(self.error("@LoadEnv path cannot contain interpolation"));
            }
            Token::StringEnd => String::new(),
            _ => {
                return Err(self.error(format!(
                    "expected string content, found {}",
                    self.peek().human_name()
                )))
            }
        };

        if self.at(&Token::InterpolStart) {
            return Err(self.error("@LoadEnv path cannot contain interpolation"));
        }

        self.expect(&Token::StringEnd)?;
        Ok(path)
    }

    fn parse_top_level_item(&mut self) -> Result<TopLevelItem, SparError> {
        match self.peek() {
            Token::Import     => Ok(TopLevelItem::Import(self.parse_import()?)),
            Token::Var        => Ok(TopLevelItem::Var(self.parse_var_decl(false)?)),
            Token::Dynamic    => Ok(TopLevelItem::Dynamic(self.parse_dynamic_decl()?)),
            Token::LBracket   => self.parse_section(false, false),
            Token::KwFunction => Ok(TopLevelItem::Function(
                self.parse_top_level_function_decl(false)?,
            )),
            Token::Ident(s) if s == "type" => Ok(TopLevelItem::Type(self.parse_type_decl(false)?)),
            Token::Ident(s) if s == "enum" => Ok(TopLevelItem::Enum(self.parse_enum_decl(false)?)),
            Token::Ident(s) if s == "functionGroup" => Ok(TopLevelItem::FunctionGroup(self.parse_function_group_decl(false)?)),
            Token::Ident(s) if s == "Schema" => Ok(TopLevelItem::SchemaSection(self.parse_schema_decl()?)),
            Token::Ident(s) if s == "SchemaFrom" => Ok(TopLevelItem::SchemaFrom(self.parse_schema_from_decl()?)),
            Token::Ident(s) if s == "task" => Ok(TopLevelItem::Task(Box::new(self.parse_task_decl()?))),
            Token::Export => {
                self.advance();
                match self.peek() {
                    Token::Var      => Ok(TopLevelItem::Var(self.parse_var_decl(true)?)),
                    Token::LBracket => self.parse_section(true, false),
                    Token::Ident(s) if s == "type" => Ok(TopLevelItem::Type(self.parse_type_decl(true)?)),
                    Token::Ident(s) if s == "enum" => Ok(TopLevelItem::Enum(self.parse_enum_decl(true)?)),
                    _ => Err(self.error(format!("expected 'var', 'type', 'enum', or '[' after 'export', found {}", self.peek().human_name()))),
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
                        let item = self.parse_section(false, true)?;
                        Ok(item)
                    }
                    Token::KwFunction => {
                        Ok(TopLevelItem::Function(
                            self.parse_top_level_function_decl(true)?,
                        ))
                    }
                    Token::Ident(s) if s == "functionGroup" => {
                        Ok(TopLevelItem::FunctionGroup(self.parse_function_group_decl(true)?))
                    }
                    _ => {
                        Err(self.error(format!(
                            "'private' must be followed by 'function', 'functionGroup', or a section declaration '[SectionName]{{...}}', found {}",
                            self.peek().human_name()
                        )))
                    }
                }
            }
            Token::At => Err(self.error(
                "'@SchemaFile' or '@LoadEnv' pragma must be the first item in the file; \
                 pragmas cannot appear mid-file"
            )),
            _ => Err(self.error(format!(
                "unexpected {}: expected 'import', 'var', 'export', 'dynamic', 'private', 'function', 'functionGroup', 'type', 'Schema', 'task', or '[' to start a declaration",
                self.peek().human_name()
            ))),
        }
    }

    fn parse_import(&mut self) -> Result<ImportDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::Import)?;

        // `import schema "path";`
        if matches!(self.peek(), Token::Ident(s) if s == "schema") {
            self.advance();
            let path = self.parse_import_path()?;
            self.expect(&Token::Semicolon)?;
            return Ok(ImportDecl {
                path,
                kind: ImportKind::Schema,
                span,
            });
        }

        // `import asPartOf "path";`
        if matches!(self.peek(), Token::Ident(s) if s == "asPartOf") {
            self.advance();
            let path = self.parse_import_path()?;
            self.expect(&Token::Semicolon)?;
            return Ok(ImportDecl {
                path,
                kind: ImportKind::AsPartOf,
                span,
            });
        }

        // `import type { A, B } from "path";`
        if matches!(self.peek(), Token::Ident(s) if s == "type") {
            self.advance();
            let items = self.parse_import_items()?;
            self.expect_from_keyword()?;
            let path = self.parse_import_path()?;
            self.expect(&Token::Semicolon)?;
            return Ok(ImportDecl {
                path,
                kind: ImportKind::TypeSelective(items),
                span,
            });
        }

        // `import { A, B as C } from "path";`
        if self.at(&Token::LBrace) {
            let items = self.parse_import_items()?;
            self.expect_from_keyword()?;
            let path = self.parse_import_path()?;
            self.expect(&Token::Semicolon)?;
            return Ok(ImportDecl {
                path,
                kind: ImportKind::Selective(items),
                span,
            });
        }

        // `import "path" [as alias];`
        let path = self.parse_import_path()?;
        let alias = if self.at(&Token::As) {
            self.advance();
            let (name, _) = self.expect_ident()?;
            Some(name)
        } else {
            None
        };
        self.expect(&Token::Semicolon)?;
        Ok(ImportDecl {
            path,
            kind: ImportKind::Aliased(alias),
            span,
        })
    }

    fn parse_import_items(&mut self) -> Result<Vec<ImportItem>, SparError> {
        self.expect(&Token::LBrace)?;
        let mut items = Vec::new();
        loop {
            if self.at(&Token::RBrace) {
                break;
            }
            let item_span = self.peek_span();
            let (name, name_span) = self.expect_ident()?;
            let alias = if self.at(&Token::As) {
                self.advance();
                let (a, _) = self.expect_ident()?;
                Some(a)
            } else {
                None
            };
            items.push(ImportItem {
                name,
                name_span,
                alias,
                span: item_span,
            });
            if self.at(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&Token::RBrace)?;
        if items.is_empty() {
            return Err(self.error(
                "selective import must name at least one item — \
                 use `import \"path\" as alias;` to import a whole file",
            ));
        }
        Ok(items)
    }

    fn expect_from_keyword(&mut self) -> Result<(), SparError> {
        match self.peek() {
            Token::Ident(s) if s == "from" => {
                self.advance();
                Ok(())
            }
            _ => Err(self.error(format!(
                "expected 'from' after import list, found {}",
                self.peek().human_name()
            ))),
        }
    }

    fn parse_import_path(&mut self) -> Result<String, SparError> {
        self.expect(&Token::StringStart)?;

        let content = match self.peek() {
            Token::StringFragment(_) => {
                let st = self.advance().clone();
                if let Token::StringFragment(s) = st.token {
                    s
                } else {
                    unreachable!()
                }
            }
            Token::InterpolStart => {
                return Err(self.error("import paths cannot contain interpolation"));
            }
            Token::StringEnd => String::new(),
            _ => {
                return Err(self.error(format!(
                    "expected string content, found {}",
                    self.peek().human_name()
                )))
            }
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
        Ok(VarDecl {
            exported,
            name,
            optional,
            ty,
            value,
            span,
        })
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
        Ok(DynamicDecl {
            name,
            optional,
            value,
            span,
        })
    }

    fn parse_section_item(&mut self) -> Result<SectionItem, SparError> {
        match self.peek() {
            Token::DotDotDot => Ok(SectionItem::Spread(self.parse_spread()?)),
            Token::Ident(_) => Ok(SectionItem::Field(self.parse_field_decl()?)),
            _ => Err(self.error("expected a field declaration or `...` spread")),
        }
    }

    /// Does a type follow at the current position? A dedicated type
    /// keyword (`str`/`int`/`float`/`bool`/`section`), or `[` immediately
    /// followed by one — used to disambiguate `name: type = value;` from
    /// the type-omitted `name: value;` form (see `parse_field_decl`).
    /// Safe because primitives are their own tokens, never `Token::Ident`
    /// — a value can never start with one of these.
    fn at_type_start(&self) -> bool {
        match self.peek() {
            Token::TypeStr
            | Token::TypeInt
            | Token::TypeFloat
            | Token::TypeBool
            | Token::TypeSection => true,
            // A bare Ident is only a type-start when immediately followed by
            // `=` — otherwise it's the type-omitted value form (`name: someVar;`).
            Token::Ident(_) => matches!(
                self.tokens.get(self.pos + 1).map(|st| &st.token),
                Some(Token::Eq)
            ),
            Token::LBracket => {
                matches!(
                    self.tokens.get(self.pos + 1).map(|st| &st.token),
                    Some(Token::TypeStr)
                        | Some(Token::TypeInt)
                        | Some(Token::TypeFloat)
                        | Some(Token::TypeBool)
                ) || (
                    // `[Ident] =` — list of a named type, same `=`-disambiguation.
                    matches!(
                        self.tokens.get(self.pos + 1).map(|st| &st.token),
                        Some(Token::Ident(_))
                    ) && matches!(
                        self.tokens.get(self.pos + 2).map(|st| &st.token),
                        Some(Token::RBracket)
                    ) && matches!(
                        self.tokens.get(self.pos + 3).map(|st| &st.token),
                        Some(Token::Eq)
                    )
                )
            }
            _ => false,
        }
    }

    /// Parse a section field. Two forms:
    /// - `name: type = value;` (or `name: type;` with no value) — type
    ///   always explicit, legal in any section.
    /// - `name: value;` — type omitted, inferred from the enclosing
    ///   section's `-> TypeName` binding (a typechecker concern; the
    ///   parser accepts this form unconditionally). No `=` in this form.
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

        if self.at_type_start() {
            let ty = self.parse_type()?;
            let value = if self.at(&Token::Eq) {
                self.advance();
                if ty == SparType::Section && self.at(&Token::LBrace) {
                    // Parse inline section body: '{' section_item* '}'
                    self.expect(&Token::LBrace)?;
                    let mut items = Vec::new();
                    while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                        items.push(self.parse_section_item()?);
                    }
                    self.expect(&Token::RBrace)?;
                    Some(FieldValue::Nested(items))
                } else {
                    Some(FieldValue::Expr(self.parse_expr()?))
                }
            } else {
                None
            };
            self.expect(&Token::Semicolon)?;
            Ok(FieldDecl {
                name,
                optional,
                ty: Some(ty),
                value,
                span,
            })
        } else {
            let value = if self.at(&Token::LBrace) {
                self.expect(&Token::LBrace)?;
                let mut items = Vec::new();
                while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                    items.push(self.parse_section_item()?);
                }
                self.expect(&Token::RBrace)?;
                Some(FieldValue::Nested(items))
            } else {
                Some(FieldValue::Expr(self.parse_expr()?))
            };
            self.expect(&Token::Semicolon)?;
            Ok(FieldDecl {
                name,
                optional,
                ty: None,
                value,
                span,
            })
        }
    }

    fn parse_spread(&mut self) -> Result<SpreadStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::DotDotDot)?;
        let expr = self.parse_expr()?;
        self.expect(&Token::Semicolon)?;
        Ok(SpreadStmt { expr, span })
    }

    /// Parse `Schema [Name]{ ... }` or `Schema? [Name]{ ... }`. The `Schema`
    /// ident itself is only `peek()`ed by the caller's dispatch — consume it
    /// here.
    fn parse_schema_decl(&mut self) -> Result<SchemaSectionDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'Schema' ident
        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&Token::LBracket)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&Token::RBracket)?;
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            fields.push(self.parse_schema_field()?);
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        Ok(SchemaSectionDecl {
            name,
            marker: SchemaMarker { optional },
            fields,
            span,
        })
    }

    fn parse_schema_from_decl(&mut self) -> Result<SchemaFromDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume 'SchemaFrom' ident

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        self.expect(&Token::LBracket)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&Token::Comma)?;
        let (source_type, source_type_span) = self.expect_ident()?;
        self.expect(&Token::RBracket)?;
        self.expect(&Token::Semicolon)?;

        Ok(SchemaFromDecl {
            name,
            source_type,
            source_type_span,
            marker: SchemaMarker { optional },
            span,
        })
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
        Ok(SchemaField {
            name,
            optional,
            shape,
            span,
        })
    }

    /// Parse `type [Name]{ ... }`. The caller only `peek()`ed the `type`
    /// ident to dispatch here — it hasn't been consumed yet, so this
    /// function consumes it first.
    fn parse_enum_decl(&mut self, exported: bool) -> Result<EnumDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'enum' ident
        let (name, name_span) = self.expect_ident()?;
        self.expect(&Token::LBrace)?;
        let mut variants = Vec::new();
        if !self.at(&Token::RBrace) {
            let (v, _) = self.expect_ident()?;
            variants.push(v);
            while self.at(&Token::Comma) {
                self.advance();
                if self.at(&Token::RBrace) {
                    break; // trailing comma
                }
                let (v, _) = self.expect_ident()?;
                variants.push(v);
            }
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        Ok(EnumDecl {
            name,
            name_span,
            exported,
            variants,
            span,
        })
    }

    fn parse_type_decl(&mut self, exported: bool) -> Result<TypeDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'type' ident
        self.expect(&Token::LBracket)?;
        let (name, name_span) = self.expect_ident()?;
        self.expect(&Token::RBracket)?;
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            fields.push(self.parse_type_field()?);
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        Ok(TypeDecl {
            name,
            name_span,
            exported,
            fields,
            span,
        })
    }

    /// Parse a single type field: `name: Type;`, `name?: Type;`,
    /// `name: OtherDeclaredType;`, or `name: section = { ... };`.
    fn parse_type_field(&mut self) -> Result<TypeField, SparError> {
        let span = self.peek_span();
        let (name, _) = self.expect_ident()?;

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        self.expect(&Token::Colon)?;
        let shape = self.parse_type_field_shape()?;
        self.expect(&Token::Semicolon)?;
        Ok(TypeField {
            name,
            optional,
            shape,
            span,
        })
    }

    fn parse_type_field_shape(&mut self) -> Result<TypeFieldShape, SparError> {
        if self.at(&Token::TypeSection) {
            self.advance(); // consume 'section'
            self.expect(&Token::Eq)?;
            self.expect(&Token::LBrace)?;
            let mut nested = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                nested.push(self.parse_type_field()?);
            }
            self.expect(&Token::RBrace)?;
            Ok(TypeFieldShape::Section(nested))
        } else if let Token::Ident(name) = self.peek() {
            // 'str'/'int'/'float'/'bool'/'section' are their own dedicated
            // tokens (see parse_scalar_type) — any Ident here is
            // unambiguously a reference to another declared type.
            let name = name.clone();
            self.advance();
            Ok(TypeFieldShape::Named(name))
        } else {
            let ty = self.parse_type()?;
            Ok(TypeFieldShape::Primitive(ty))
        }
    }

    fn parse_section(&mut self, exported: bool, private: bool) -> Result<TopLevelItem, SparError> {
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

        // Check for a `-> TypeName` binding BEFORE the `{`
        let type_binding = self.try_parse_type_binding()?;

        let items = self.parse_regular_section_items()?;
        Ok(TopLevelItem::Section(SectionDecl {
            exported,
            private,
            path: vec![name],
            items,
            type_binding,
            span,
        }))
    }

    /// Try to consume `-> TypeName` after a section name. Returns `None` if
    /// there's no `->` at all (leaves the token stream unchanged) — matches
    /// the arrow already used for function return types.
    fn try_parse_type_binding(&mut self) -> Result<Option<TypeBinding>, SparError> {
        if !self.at(&Token::Arrow) {
            return Ok(None);
        }
        self.advance(); // consume '->'
        let (name, name_span) = self.expect_ident()?;
        Ok(Some(TypeBinding {
            name,
            span: name_span,
        }))
    }

    /// Parse a regular section body: `{ ...fields/spreads... };`, including
    /// the trailing `;`.
    fn parse_regular_section_items(&mut self) -> Result<Vec<SectionItem>, SparError> {
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
        Ok(items)
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
        if let Token::Ident(name) = self.peek() {
            // 'str'/'int'/'float'/'bool'/'section' are their own dedicated
            // tokens (see below) — any Ident here is unambiguously a
            // reference to another declared type. Mirrors
            // parse_type_field_shape's identical handling one level up
            // (inside `type [X]{...}` field shapes).
            let name = name.clone();
            self.advance();
            return Ok(SparType::Named(name));
        }
        let ty = match self.peek() {
            Token::TypeStr     => SparType::Str,
            Token::TypeInt     => SparType::Int,
            Token::TypeFloat   => SparType::Float,
            Token::TypeBool    => SparType::Bool,
            Token::TypeSection => SparType::Section,
            _ => return Err(self.error(format!("expected a type ('str', 'int', 'float', 'bool', 'section', or a declared type name), found {}", self.peek().human_name()))),
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
                Token::EqEq => BinOp::Eq,
                Token::NotEq => BinOp::NotEq,
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::LtEq => BinOp::LtEq,
                Token::GtEq => BinOp::GtEq,
                _ => break,
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
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
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
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
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
            Token::LBrace => self.parse_object_literal(),
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
            _ => Err(self.error(format!(
                "expected an expression, found {}",
                self.peek().human_name()
            ))),
        }?;

        // Postfix indexing/field-access: expr[index], expr.field — one
        // loop so both compose freely in any order (a.b[0], a[0].b, a.b.c).
        while self.at(&Token::LBracket) || self.at(&Token::Dot) {
            if self.at(&Token::LBracket) {
                let span = self.peek_span();
                self.advance();
                let index = self.parse_expr()?;
                self.expect(&Token::RBracket)?;
                expr = Expr::Index {
                    source: Box::new(expr),
                    index: Box::new(index),
                    span,
                };
            } else {
                let span = self.peek_span();
                self.advance(); // consume '.'
                let (field, field_span) = self.expect_ident()?;
                expr = Expr::FieldAccess {
                    base: Box::new(expr),
                    field,
                    field_span,
                    span,
                };
            }
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

    fn parse_object_literal(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        self.expect(&Token::LBrace)?;
        let mut items = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            items.push(self.parse_section_item()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(Expr::Object(items, span))
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
                _ => {
                    return Err(self.error(format!(
                        "unexpected {} inside string",
                        self.peek().human_name()
                    )))
                }
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
        Ok(Expr::Call {
            name,
            name_span,
            args,
            span,
        })
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
            params.push(Param {
                name: param_name,
                ty,
                span: param_span,
            });
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
        while self.at(&Token::Var)
            || self.at(&Token::KwIf)
            || self.at(&Token::KwReturn)
            || self.at(&Token::KwFor)
        {
            stmts.push(self.parse_func_stmt()?);
        }
        let body_span = self.peek_span();
        self.expect(&Token::RBrace)?;
        Ok(FunctionDecl {
            name,
            name_span,
            params,
            ret,
            ret_span,
            body: FunctionBody {
                stmts,
                span: body_span,
            },
            is_private,
            span,
        })
    }

    fn parse_top_level_function_decl(
        &mut self,
        is_private: bool,
    ) -> Result<FunctionDecl, SparError> {
        let decl = self.parse_function_decl(is_private)?;
        self.expect(&Token::Semicolon)?;
        Ok(decl)
    }

    fn parse_function_group_decl(
        &mut self,
        is_private: bool,
    ) -> Result<FunctionGroupDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'functionGroup' ident
        let (name, name_span) = self.expect_ident()?;
        self.expect(&Token::LBrace)?;
        let mut functions = Vec::new();
        while self.at(&Token::KwFunction) || self.at(&Token::Private) {
            if self.at(&Token::Private) {
                self.advance();
                functions.push(self.parse_function_decl(true)?);
            } else {
                functions.push(self.parse_function_decl(false)?);
            }
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        Ok(FunctionGroupDecl {
            is_private,
            name,
            name_span,
            functions,
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
                    let (ty, value) = if self.at_type_start() {
                        let ty = self.parse_type()?;
                        self.expect(&Token::Eq)?;
                        (Some(ty), self.parse_or()?)
                    } else {
                        (None, self.parse_or()?)
                    };
                    self.expect(&Token::Semicolon)?;
                    fields.push(ReturnField {
                        name: field_name,
                        ty,
                        value,
                        span: field_span,
                    });
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
        Ok(FuncStmt::For {
            var_name,
            iterable,
            body,
            span,
        })
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
        Ok(LocalVarDecl {
            name,
            ty,
            value,
            span,
        })
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
        Ok(IfStmt {
            condition,
            then_stmts,
            else_stmts,
            span,
        })
    }

    fn parse_task_decl(&mut self) -> Result<TaskDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'task' ident
        self.expect(&Token::LBracket)?;
        let (name, name_span) = self.expect_ident()?;
        self.expect(&Token::RBracket)?;

        let params = if self.at(&Token::LParen) {
            self.advance();
            let mut params = Vec::new();
            let mut saw_default = false;
            let mut saw_variadic = false;
            while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
                if saw_variadic {
                    return Err(self.error("a variadic task parameter must be last"));
                }
                let param_span = self.peek_span();
                let variadic = if self.at(&Token::Star) {
                    self.advance();
                    true
                } else {
                    false
                };
                let (param_name, _) = self.expect_ident()?;
                self.expect(&Token::Colon)?;
                let ty = self.parse_type()?;
                let default = if self.at(&Token::Eq) {
                    if variadic {
                        return Err(self.error("a variadic task parameter cannot have a default"));
                    }
                    self.advance();
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if default.is_none() && saw_default && !variadic {
                    return Err(self.error(
                        "a required task parameter cannot follow a parameter with a default",
                    ));
                }
                saw_default |= default.is_some();
                saw_variadic = variadic;
                params.push(TaskParam {
                    name: param_name,
                    ty,
                    default,
                    variadic,
                    span: param_span,
                });
                if self.at(&Token::Comma) {
                    self.advance();
                }
            }
            self.expect(&Token::RParen)?;
            params
        } else {
            Vec::new()
        };

        self.expect(&Token::LBrace)?;

        let mut description = None;
        let mut default = None;
        let mut quiet = None;
        let mut private = None;
        let mut group = None;
        let mut confirm = None;
        let mut depends_on = Vec::new();
        let mut env = Vec::new();
        let mut cwd = None;
        let mut shell = None;
        let mut run_blocks: Vec<RunBlock> = Vec::new();
        let mut seen_run_labels: std::collections::HashSet<Option<String>> = std::collections::HashSet::new();

        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            let (field_name, field_span) = if self.at(&Token::Private) {
                let field_span = self.peek_span();
                self.advance();
                ("private".to_string(), field_span)
            } else {
                self.expect_ident()?
            };
            match field_name.as_str() {
                "run" => {
                    let (os_label, os_span) = if let Token::Ident(label) = self.peek().clone() {
                        let label_span = self.peek_span();
                        self.advance();
                        (Some(label), Some(label_span))
                    } else {
                        (None, None)
                    };
                    let run_start = field_span.clone();
                    let commands = self.parse_run_block()?;
                    if !seen_run_labels.insert(os_label.clone()) {
                        let message = match &os_label {
                            Some(label) => format!("task 'run {label}' block may only appear once"),
                            None => "task can only have one default 'run {}' block".to_string(),
                        };
                        return Err(SparError::ParseError { message, span: run_start });
                    }
                    run_blocks.push(RunBlock {
                        os: os_label,
                        os_span,
                        commands,
                        span: run_start,
                    });
                }
                "description" => {
                    self.expect(&Token::Colon)?;
                    description = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "default" => {
                    self.expect(&Token::Colon)?;
                    default = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "quiet" => {
                    self.expect(&Token::Colon)?;
                    quiet = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "private" => {
                    self.expect(&Token::Colon)?;
                    private = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "group" => {
                    self.expect(&Token::Colon)?;
                    group = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "confirm" => {
                    self.expect(&Token::Colon)?;
                    confirm = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "cwd" => {
                    self.expect(&Token::Colon)?;
                    cwd = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "shell" => {
                    self.expect(&Token::Colon)?;
                    shell = Some(self.parse_expr()?);
                    self.expect(&Token::Semicolon)?;
                }
                "dependsOn" => {
                    self.expect(&Token::Colon)?;
                    self.expect(&Token::LBracket)?;
                    if !self.at(&Token::RBracket) {
                        let (dep_name, dep_span) = self.expect_ident()?;
                        depends_on.push(TaskRef {
                            name: dep_name,
                            span: dep_span,
                        });
                        while self.at(&Token::Comma) {
                            self.advance();
                            if self.at(&Token::RBracket) {
                                break;
                            }
                            let (dep_name, dep_span) = self.expect_ident()?;
                            depends_on.push(TaskRef {
                                name: dep_name,
                                span: dep_span,
                            });
                        }
                    }
                    self.expect(&Token::RBracket)?;
                    self.expect(&Token::Semicolon)?;
                }
                "env" => {
                    self.expect(&Token::Colon)?;
                    self.expect(&Token::LBrace)?;
                    while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                        let (key, _) = self.expect_ident()?;
                        self.expect(&Token::Colon)?;
                        let value = self.parse_expr()?;
                        self.expect(&Token::Semicolon)?;
                        env.push((key, value));
                    }
                    self.expect(&Token::RBrace)?;
                    self.expect(&Token::Semicolon)?;
                }
                other => {
                    return Err(SparError::ParseError {
                        message: format!(
                            "unknown task field '{other}'; expected 'description', 'default', 'quiet', 'private', 'group', 'confirm', 'dependsOn', 'cwd', 'shell', 'env', or 'run'"
                        ),
                        span: field_span,
                    });
                }
            }
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;

        if run_blocks.is_empty() {
            return Err(SparError::ParseError {
                message: format!("task '{name}' must declare at least one 'run' block"),
                span,
            });
        }

        Ok(TaskDecl {
            name,
            name_span,
            params,
            description,
            default,
            quiet,
            private,
            group,
            confirm,
            depends_on,
            env,
            cwd,
            shell,
            run_blocks,
            span,
        })
    }

    /// Parses a task's `run { ... };` body. The lexer has already turned the
    /// opening `{` into `RunStart` and will terminate the body with
    /// `RunEnd`; between those, `ShellFragment` text and `${expr}`
    /// interpolation islands (`InterpolStart`/`InterpolEnd`, same tokens
    /// string interpolation uses) alternate. This function splits the
    /// flattened part sequence into one `ShellCommand` per top-level `;`
    /// (i.e. a `;` that isn't inside a single- or double-quoted shell
    /// string) — mirroring how each line of a Just recipe is one shell
    /// invocation, echoed and failure-checked independently.
    fn parse_run_block(&mut self) -> Result<Vec<ShellCommand>, SparError> {
        self.expect(&Token::RunStart)?;
        let block_span = self.peek_span();

        enum RawPart {
            Text(String),
            Expr(Expr),
        }
        let mut raw: Vec<RawPart> = Vec::new();
        loop {
            match self.peek().clone() {
                Token::ShellFragment(s) => {
                    self.advance();
                    raw.push(RawPart::Text(s));
                }
                Token::InterpolStart => {
                    self.advance();
                    let expr = self.parse_expr()?;
                    self.expect(&Token::InterpolEnd)?;
                    raw.push(RawPart::Expr(expr));
                }
                Token::RunEnd => {
                    self.advance();
                    break;
                }
                _ => {
                    return Err(self.error(format!(
                        "unexpected {} inside run block",
                        self.peek().human_name()
                    )))
                }
            }
        }
        self.expect(&Token::Semicolon)?;

        let mut leading_literal = String::new();
        for part in &raw {
            match part {
                RawPart::Text(text) => leading_literal.push_str(text),
                RawPart::Expr(_) => break,
            }
        }
        let is_shebang = leading_literal.trim_start().starts_with("#!");

        let mut commands: Vec<ShellCommand> = Vec::new();
        if is_shebang {
            let mut parts = raw
                .into_iter()
                .map(|part| match part {
                    RawPart::Text(text) => ShellTemplatePart::Literal(text),
                    RawPart::Expr(expr) => ShellTemplatePart::Expr(expr),
                })
                .collect();
            push_shell_command(&mut parts, &mut commands, &block_span, true);
        } else {
            let mut current: Vec<ShellTemplatePart> = Vec::new();
            let mut literal = String::new();
            let mut in_single = false;
            let mut in_double = false;

            for part in raw {
                match part {
                    RawPart::Expr(e) => {
                        if !literal.is_empty() {
                            current.push(ShellTemplatePart::Literal(std::mem::take(&mut literal)));
                        }
                        current.push(ShellTemplatePart::Expr(e));
                    }
                    RawPart::Text(s) => {
                        let mut chars = s.chars().peekable();
                        while let Some(c) = chars.next() {
                            match c {
                                '\\' => {
                                    literal.push(c);
                                    if let Some(next) = chars.next() {
                                        literal.push(next);
                                    }
                                }
                                '\'' if !in_double => {
                                    in_single = !in_single;
                                    literal.push(c);
                                }
                                '"' if !in_single => {
                                    in_double = !in_double;
                                    literal.push(c);
                                }
                                ';' if !in_single && !in_double => {
                                    if !literal.is_empty() {
                                        current.push(ShellTemplatePart::Literal(std::mem::take(
                                            &mut literal,
                                        )));
                                    }
                                    push_shell_command(
                                        &mut current,
                                        &mut commands,
                                        &block_span,
                                        false,
                                    );
                                }
                                _ => literal.push(c),
                            }
                        }
                    }
                }
            }
            if !literal.is_empty() {
                current.push(ShellTemplatePart::Literal(literal));
            }
            push_shell_command(&mut current, &mut commands, &block_span, false);
        }

        if commands.is_empty() {
            return Err(SparError::ParseError {
                message: "task 'run' block must contain at least one command".to_string(),
                span: block_span,
            });
        }
        Ok(commands)
    }
}

/// Trims leading/trailing whitespace-only literal parts and, if anything
/// meaningful remains, appends one `ShellCommand` built from `parts` (which
/// is left empty for the caller to reuse).
fn push_shell_command(
    parts: &mut Vec<ShellTemplatePart>,
    commands: &mut Vec<ShellCommand>,
    span: &Span,
    is_shebang: bool,
) {
    if let Some(ShellTemplatePart::Literal(s)) = parts.first_mut() {
        *s = s.trim_start().to_string();
    }
    if let Some(ShellTemplatePart::Literal(s)) = parts.last_mut() {
        *s = s.trim_end().to_string();
    }
    parts.retain(|p| !matches!(p, ShellTemplatePart::Literal(s) if s.is_empty()));
    if !parts.is_empty() {
        commands.push(ShellCommand {
            parts: std::mem::take(parts),
            is_shebang,
            span: span.clone(),
        });
    } else {
        parts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        BinOp, Expr, FieldValue, FnCall, Literal, SectionItem, SparType, StringPart, TopLevelItem,
    };

    fn parse_str(src: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(src)
            .tokenize()
            .expect("lex failed");
        Parser::new(tokens).parse().expect("parse failed")
    }

    fn parse_err(src: &str) -> String {
        let tokens = crate::lexer::Lexer::new(src)
            .tokenize()
            .expect("lex failed");
        Parser::new(tokens).parse().unwrap_err().to_string()
    }

    fn first_item(src: &str) -> TopLevelItem {
        parse_str(src).items.into_iter().next().expect("no items")
    }

    #[test]
    fn test_import_no_alias() {
        let item = first_item(r#"import "base.spar";"#);
        let TopLevelItem::Import(decl) = item else {
            panic!("not import")
        };
        assert_eq!(decl.path, "base.spar");
        assert!(matches!(decl.kind, ImportKind::Aliased(None)));
    }

    #[test]
    fn test_import_with_alias() {
        let item = first_item(r#"import "config/base.spar" as config;"#);
        let TopLevelItem::Import(decl) = item else {
            panic!("not import")
        };
        assert_eq!(decl.path, "config/base.spar");
        assert!(matches!(decl.kind, ImportKind::Aliased(Some(ref a)) if a == "config"));
    }

    #[test]
    fn test_import_interpolation_error() {
        let err = parse_err(r#"import "${bad}.spar";"#);
        assert!(
            err.contains("import paths cannot contain interpolation"),
            "got: {err}"
        );
    }

    #[test]
    fn test_required_var_decl() {
        let item = first_item("var port: int = 3000;");
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
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
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        assert!(decl.optional);
        assert_eq!(decl.ty, SparType::Str);
        assert!(decl.value.is_none());
    }

    #[test]
    fn test_exported_var() {
        let item = first_item(r#"export var version: str = "1.0.0";"#);
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        assert!(decl.exported);
        assert_eq!(decl.name, "version");
    }

    #[test]
    fn test_typed_list_var() {
        let item = first_item("var ports: [int] = [3000, 8080];");
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        assert_eq!(decl.ty, SparType::List(Box::new(SparType::Int)));
        assert!(matches!(decl.value, Some(Expr::List(_, _))));
    }

    #[test]
    fn test_dynamic_var_with_value() {
        let item = first_item(r#"dynamic var tags = [2026, "prod", true];"#);
        let TopLevelItem::Dynamic(decl) = item else {
            panic!("not dynamic")
        };
        assert_eq!(decl.name, "tags");
        assert!(!decl.optional);
        assert!(matches!(decl.value, Some(Expr::List(_, _))));
    }

    #[test]
    fn test_dynamic_var_optional_no_value() {
        let item = first_item("dynamic var meta?;");
        let TopLevelItem::Dynamic(decl) = item else {
            panic!("not dynamic")
        };
        assert!(decl.optional);
        assert!(decl.value.is_none());
    }

    #[test]
    fn test_dynamic_non_list_error() {
        let err = parse_err("dynamic var bad = 3000;");
        assert!(
            err.contains("dynamic variables must be assigned a list literal"),
            "got: {err}"
        );
    }

    #[test]
    fn test_simple_section() {
        let item = first_item("[server]{ port: int = 3000; };");
        let TopLevelItem::Section(decl) = item else {
            panic!("not section")
        };
        assert!(!decl.exported);
        assert_eq!(decl.path, vec!["server"]);
        assert_eq!(decl.items.len(), 1);
        assert!(matches!(decl.items[0], SectionItem::Field(_)));
    }

    #[test]
    fn test_exported_section() {
        let item = first_item("export [defaults]{ workers: int = 4; };");
        let TopLevelItem::Section(decl) = item else {
            panic!("not section")
        };
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
        let TopLevelItem::Section(decl) = item else {
            panic!("not section")
        };
        let SectionItem::Field(f) = &decl.items[0] else {
            panic!("not field")
        };
        assert!(f.optional);
        assert!(f.value.is_none());
    }

    #[test]
    fn test_local_spread() {
        let item = first_item("[project]{ ...base_project; };");
        let TopLevelItem::Section(decl) = item else {
            panic!("not section")
        };
        let SectionItem::Spread(s) = &decl.items[0] else {
            panic!("not spread")
        };
        let Expr::NamespaceRef(nr) = &s.expr else {
            panic!("expected NamespaceRef")
        };
        assert_eq!(nr.segments, vec!["base_project"]);
    }

    #[test]
    fn test_namespaced_spread() {
        let item = first_item("[project]{ ...global::base_project; };");
        let TopLevelItem::Section(decl) = item else {
            panic!("not section")
        };
        let SectionItem::Spread(s) = &decl.items[0] else {
            panic!("not spread")
        };
        let Expr::NamespaceRef(nr) = &s.expr else {
            panic!("expected NamespaceRef")
        };
        assert_eq!(nr.segments, vec!["global", "base_project"]);
    }

    #[test]
    fn test_arithmetic_expr() {
        let item = first_item("var timeout: int = 30 * 3;");
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        let Some(Expr::BinaryOp(op)) = decl.value else {
            panic!("not binop")
        };
        assert_eq!(op.op, BinOp::Mul);
        assert!(matches!(*op.lhs, Expr::Literal(Literal::Int(30))));
        assert!(matches!(*op.rhs, Expr::Literal(Literal::Int(3))));
    }

    #[test]
    fn test_fallback_expr() {
        let item = first_item(r#"var port: int = env("PORT") ?? 3000;"#);
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        let Some(Expr::BinaryOp(op)) = decl.value else {
            panic!("not binop")
        };
        assert_eq!(op.op, BinOp::Fallback);
        assert!(matches!(*op.lhs, Expr::FnCall(FnCall { ref name, .. }) if name == "env"));
        assert!(matches!(*op.rhs, Expr::Literal(Literal::Int(3000))));
    }

    #[test]
    fn test_namespace_ref() {
        let item = first_item("var x: int = global::port;");
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        let Some(Expr::NamespaceRef(nr)) = decl.value else {
            panic!("not ns ref")
        };
        assert_eq!(nr.segments, vec!["global", "port"]);
    }

    #[test]
    fn test_interpolated_string_expr() {
        let item = first_item(r#"var url: str = "http://${global::host}";"#);
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        let Some(Expr::String(s)) = decl.value else {
            panic!("not string")
        };
        assert_eq!(s.parts.len(), 3);
        assert!(matches!(&s.parts[0], StringPart::Literal(l) if l == "http://"));
        let StringPart::Expr(e) = &s.parts[1] else {
            panic!("not expr part")
        };
        let Expr::NamespaceRef(nr) = e.as_ref() else {
            panic!("not ns ref")
        };
        assert_eq!(nr.segments, vec!["global", "host"]);
        assert!(matches!(&s.parts[2], StringPart::Literal(l) if l.is_empty()));
    }

    #[test]
    fn test_env_fn_call() {
        let item = first_item(r#"var mode: str = env("APP_MODE");"#);
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        let Some(Expr::FnCall(fc)) = decl.value else {
            panic!("not fn call")
        };
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
    fn top_level_type_requires_trailing_semicolon() {
        assert!(parse_err("type [User]{ name: str; }").contains("expected ';'"));
        parse_str("type [User]{ name: str; };");
    }

    #[test]
    fn top_level_schema_requires_trailing_semicolon() {
        let missing = "@SchemaFile\nSchema [Server]{ port: int; }";
        assert!(parse_err(missing).contains("expected ';'"));
        parse_str("@SchemaFile\nSchema [Server]{ port: int; };");
    }

    #[test]
    fn top_level_function_requires_trailing_semicolon() {
        let missing = "function answer() -> int { return 42; }";
        assert!(parse_err(missing).contains("expected ';'"));
        parse_str("function answer() -> int { return 42; };");
    }

    #[test]
    fn top_level_function_group_requires_trailing_semicolon() {
        let body = "functionGroup Math { function answer() -> int { return 42; } }";
        assert!(parse_err(body).contains("expected ';'"));
        parse_str(&format!("{body};"));
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
        assert!(
            result.is_ok(),
            "section field should parse: {:?}",
            result.err()
        );
    }

    #[test]
    fn nested_section_twice_deep_parses() {
        let src = "[A]{ b: section = { c: section = { val: int = 1; }; }; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(
            Parser::new(tokens).parse().is_ok(),
            "two-deep nesting should parse"
        );
    }

    #[test]
    fn empty_nested_section_parses() {
        let src = "[A]{ inner: section = { }; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(
            Parser::new(tokens).parse().is_ok(),
            "empty nested section should parse"
        );
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
        let TopLevelItem::Section(decl) = &prog.items[0] else {
            panic!()
        };
        let SectionItem::Field(f) = &decl.items[0] else {
            panic!()
        };
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
};
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_ok());
    }

    #[test]
    fn return_inside_both_if_and_else_parses() {
        let src = r#"
function f(debug: bool) -> int {
    if debug { return 1; } else { return 2; }
};
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
};
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
};
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
};
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

    fn task_decl(src: &str) -> TaskDecl {
        match first_item(src) {
            TopLevelItem::Task(t) => *t,
            other => panic!("expected a task declaration, got {other:?}"),
        }
    }

    #[test]
    fn minimal_task_parses() {
        let task = task_decl(
            r#"task [Build] {
    run {
        cargo build;
    };
};"#,
        );
        assert_eq!(task.name, "Build");
        assert!(task.params.is_empty());
        assert!(task.depends_on.is_empty());
        assert_eq!(task.run_blocks.len(), 1);
        assert_eq!(task.run_blocks[0].commands.len(), 1);
        assert!(!task.run_blocks[0].commands[0].is_shebang);
        assert!(matches!(
            &task.run_blocks[0].commands[0].parts[..],
            [ShellTemplatePart::Literal(s)] if s.trim() == "cargo build"
        ));
    }

    #[test]
    fn top_level_task_requires_trailing_semicolon() {
        let missing = "task [Build] { run { true; }; }";
        assert!(parse_err(missing).contains("expected ';'"));
        parse_str("task [Build] { run { true; }; };");
    }

    #[test]
    fn task_dependencies_parse() {
        let task = task_decl(
            r#"task [Test] {
    dependsOn: [Build, Lint];

    run {
        cargo test;
    };
};"#,
        );
        assert_eq!(
            task.depends_on
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["Build", "Lint"]
        );
    }

    #[test]
    fn task_parameters_parse() {
        let task = task_decl(
            r#"task [Deploy](environment: str) {
    run {
        ./deploy.sh ${environment};
    };
};"#,
        );
        assert_eq!(task.params.len(), 1);
        assert_eq!(task.params[0].name, "environment");
        assert_eq!(task.params[0].ty, SparType::Str);
        assert_eq!(task.run_blocks[0].commands.len(), 1);
        assert!(matches!(
            &task.run_blocks[0].commands[0].parts[..],
            [ShellTemplatePart::Literal(_), ShellTemplatePart::Expr(_)]
        ));
    }

    #[test]
    fn task_parameters_support_defaults_and_a_final_variadic() {
        let task = task_decl(
            r#"task [Deploy](environment: str = "staging", *extra: str) {
    run { echo ${environment} ${extra}; };
};"#,
        );
        assert_eq!(task.params.len(), 2);
        assert_eq!(task.params[0].name, "environment");
        assert_eq!(task.params[0].ty, SparType::Str);
        assert!(matches!(
            task.params[0].default,
            Some(Expr::String(ref string))
                if matches!(&string.parts[..], [StringPart::Literal(value)] if value == "staging")
        ));
        assert!(!task.params[0].variadic);
        assert_eq!(task.params[1].name, "extra");
        assert_eq!(task.params[1].ty, SparType::Str);
        assert!(task.params[1].default.is_none());
        assert!(task.params[1].variadic);
    }

    #[test]
    fn task_parameters_reject_invalid_default_and_variadic_ordering() {
        for src in [
            "task [Deploy](*first: str, *second: str) { run { echo hi; }; }",
            "task [Deploy](*extra: str, environment: str) { run { echo hi; }; }",
            "task [Deploy](*extra: str = \"x\") { run { echo hi; }; }",
            "task [Deploy](optional: str = \"x\", required: str) { run { echo hi; }; };",
        ] {
            assert!(
                Parser::new(crate::lexer::Lexer::new(src).tokenize().unwrap())
                    .parse()
                    .is_err()
            );
        }
    }

    #[test]
    fn task_env_parses() {
        let task = task_decl(
            r#"task [Server] {
    env: {
        RUST_LOG: "debug";
        PORT: "8080";
    };

    run {
        cargo run;
    };
};"#,
        );
        assert_eq!(task.env.len(), 2);
        assert_eq!(task.env[0].0, "RUST_LOG");
        assert_eq!(task.env[1].0, "PORT");
    }

    #[test]
    fn task_v2_metadata_fields_parse() {
        let task = task_decl(
            r#"task [Deploy] {
    private: true;
    group: "release";
    confirm: "Really deploy?";
    shell: ["bash", "-euo", "pipefail", "-c"];
    run { ./deploy.sh; };
};"#,
        );
        assert!(matches!(
            task.private,
            Some(Expr::Literal(Literal::Bool(true)))
        ));
        assert!(task.group.is_some());
        assert!(task.confirm.is_some());
        assert!(task.shell.is_some());
    }

    #[test]
    fn task_cwd_parses() {
        let task = task_decl(
            r#"task [Web] {
    cwd: "./web";

    run {
        npm run dev;
    };
};"#,
        );
        assert!(task.cwd.is_some());
    }

    #[test]
    fn task_description_default_quiet_parse() {
        let task = task_decl(
            r#"task [Test] {
    description: "Run the complete test suite";
    default: true;
    quiet: true;

    run {
        cargo test;
    };
};"#,
        );
        assert!(task.description.is_some());
        assert!(matches!(
            task.default,
            Some(Expr::Literal(Literal::Bool(true)))
        ));
        assert!(matches!(
            task.quiet,
            Some(Expr::Literal(Literal::Bool(true)))
        ));
    }

    #[test]
    fn multiple_run_commands_split_in_declaration_order() {
        let task = task_decl(
            r#"task [Example] {
    run {
        echo "hello world";
        cargo test --workspace;
        npm run build;
    };
};"#,
        );
        assert_eq!(task.run_blocks[0].commands.len(), 3);
    }

    #[test]
    fn shebang_run_block_is_one_verbatim_command() {
        let task = task_decl(
            r#"task [Script] {
    run {
        #!/usr/bin/env bash
        echo one
        echo two
        if true; then echo three; fi
    };
};"#,
        );
        assert_eq!(task.run_blocks[0].commands.len(), 1);
        assert!(task.run_blocks[0].commands[0].is_shebang);
        assert!(matches!(
            &task.run_blocks[0].commands[0].parts[..],
            [ShellTemplatePart::Literal(script)]
                if script.contains("if true; then echo three; fi")
        ));
    }

    #[test]
    fn load_env_pragma_defaults_to_dotenv_and_must_be_first() {
        let program = Parser::new(
            crate::lexer::Lexer::new("@LoadEnv\ntask [Build] { run { echo build; }; };")
                .tokenize()
                .unwrap(),
        )
        .parse()
        .expect("@LoadEnv must parse");
        assert_eq!(program.load_env.as_deref(), Some(".env"));
        assert!(!program.is_schema_file);
        assert!(Parser::new(
            crate::lexer::Lexer::new(
                "var name: str = \"spar\";\n@LoadEnv\ntask [Build] { run { echo build; }; };",
            )
            .tokenize()
            .unwrap(),
        )
        .parse()
        .is_err());
    }

    #[test]
    fn load_env_pragma_accepts_a_custom_path() {
        let program =
            parse_str("@LoadEnv(\".env.production\")\ntask [Build] { run { echo build; }; };");
        assert_eq!(program.load_env.as_deref(), Some(".env.production"));
    }

    #[test]
    fn unknown_file_pragma_lists_supported_names() {
        let message = parse_err("@Unknown\ntask [Build] { run { echo build; }; };");
        assert!(
            message.contains(
                "unknown file pragma `@Unknown`; only `@SchemaFile` or `@LoadEnv` is supported"
            ),
            "got: {message}"
        );
    }

    #[test]
    fn task_missing_run_block_is_a_parse_error() {
        let msg = parse_err("task [Build] {\n};");
        assert!(msg.contains("run"), "got: {msg}");
    }

    #[test]
    fn task_unknown_field_is_a_parse_error() {
        let msg = parse_err(
            r#"task [Build] {
    bogus: true;
    run { echo hi; };
};"#,
        );
        assert!(msg.contains("unknown task field"), "got: {msg}");
    }

    #[test]
    fn task_missing_task_name_brackets_is_a_parse_error() {
        assert!(parse_err("task Build { run { echo hi; }; }").contains("'['"));
    }

    #[test]
    fn task_duplicate_run_block_is_a_parse_error() {
        let msg = parse_err(
            r#"task [Build] {
    run { echo one; };
    run { echo two; };
};"#,
        );
        assert!(
            msg.contains("only have one default") || msg.contains("only appear once"),
            "got: {msg}"
        );
    }

    #[test]
    fn task_accepts_bare_and_labeled_run_blocks() {
        let src = "task [T] {\n\
            run {\n\
                echo default;\n\
            };\n\
            run windows {\n\
                echo win;\n\
            };\n\
            run linux {\n\
                echo linux;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let TopLevelItem::Task(task) = &program.items[0] else {
            panic!("expected a task");
        };
        assert_eq!(task.run_blocks.len(), 3);
        assert_eq!(task.run_blocks[0].os, None);
        assert_eq!(task.run_blocks[1].os.as_deref(), Some("windows"));
        assert_eq!(task.run_blocks[2].os.as_deref(), Some("linux"));
        assert_eq!(task.run_blocks[0].os_span, None);
        let windows_span = task.run_blocks[1]
            .os_span
            .as_ref()
            .expect("labeled block must carry a span for its label");
        let windows_text = &src[windows_span.start..windows_span.end];
        assert_eq!(
            windows_text, "windows",
            "os_span must point at the label token itself, not the 'run' keyword"
        );
    }

    #[test]
    fn task_rejects_duplicate_run_block_for_same_os() {
        let src = "task [T] {\n\
            run windows {\n\
                echo a;\n\
            };\n\
            run windows {\n\
                echo b;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let err = crate::parser::Parser::new(tokens).parse().expect_err("must reject");
        let message = format!("{err}");
        assert!(message.contains("windows"), "{message}");
    }

    #[test]
    fn task_rejects_two_default_run_blocks() {
        let src = "task [T] {\n\
            run {\n\
                echo a;\n\
            };\n\
            run {\n\
                echo b;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let err = crate::parser::Parser::new(tokens).parse().expect_err("must reject");
        let message = format!("{err}");
        assert!(message.contains("default") || message.contains("once"), "{message}");
    }

    #[test]
    fn task_requires_at_least_one_run_block() {
        let src = "task [T] {\n\
            description: \"no run at all\";\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        assert!(crate::parser::Parser::new(tokens).parse().is_err());
    }

    #[test]
    fn task_with_only_labeled_run_blocks_and_no_default_parses() {
        let src = "task [T] {\n\
            run windows {\n\
                echo win;\n\
            };\n\
            run macos {\n\
                echo mac;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let TopLevelItem::Task(task) = &program.items[0] else {
            panic!("expected a task");
        };
        assert_eq!(task.run_blocks.len(), 2);
    }
}
